use crate::{btc::LOG_TARGET, eth::EthClient};

use alloy::{
	primitives::ChainId,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BITCOIN_BOOTSTRAP_BLOCK_OFFSET, tx::PROVIDER_STALE_BTC_HEAD_TIMEOUT_MS,
	},
	eth::BootstrapState,
	utils::sub_display_format,
};
use eyre::Result;

use super::handlers::BootstrapHandler;
use crate::{btc::failover::FailoverClient as BtcClient, failover::HeadMonitor};
use alloy::network::Network;
use bitcoincore_rpc::{RpcApi, bitcoincore_rpc_json::GetRawTransactionResultVout};
use br_primitives::btc::{Event, EventMessage};
use miniscript::bitcoin::{Address, BlockHash, Txid, address::NetworkUnchecked};
use serde::Deserialize;
use serde_json::Value;
use std::{
	collections::{BTreeMap, BTreeSet},
	str::FromStr,
	sync::Arc,
};
use tokio::{
	sync::broadcast::{self, Receiver, Sender},
	time::{Duration, interval, sleep},
};
use tokio_stream::{StreamExt, wrappers::IntervalStream};

const SUB_LOG_TARGET: &str = "block-manager";

/// How far (in blocks) a provider's head may trail the last processed block before the provider
/// is considered behind. Right after start the processed block is the head itself, so this
/// absorbs load-balanced backends a block or two apart.
const BTC_HEAD_LAG_TOLERANCE: u64 = 2;

/// How many recently processed block hashes are kept to detect reorgs.
/// shortcut: a reorg deeper than this rescans only the tracked window; raise it if reorgs get
/// deeper (testnet4 has shown up to 45 blocks).
const REORG_TRACK_DEPTH: usize = 100;

/// A module that reads every new Bitcoin block and filters `Inbound`, `Outbound` events.
pub struct BlockManager<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The Bitcoin client.
	btc_client: BtcClient,
	/// The Bifrost client.
	pub bfc_client: Arc<EthClient<F, P, N>>,
	/// The event message sender.
	sender: Sender<EventMessage>,
	/// The configured minimum block confirmations required to process a block.
	block_confirmations: u64,
	/// The block that is waiting for confirmations.
	waiting_block: u64,
	/// Hashes of the most recently processed blocks, used to detect reorgs.
	processed_hashes: BTreeMap<u64, BlockHash>,
	/// The `getblockcount` request interval in milliseconds.
	call_interval: u64,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The bootstrap offset in blocks.
	bootstrap_offset: u32,
	/// The API endpoint for fetching Bitcoin block height.
	block_height_api: &'static str,
	/// Rotates away from a provider whose head is behind or stuck.
	head_monitor: HeadMonitor,
}

#[async_trait::async_trait]
impl<F, P, N: Network> RpcApi for BlockManager<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	async fn call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> bitcoincore_rpc::Result<T> {
		// retried and failed over by the client
		self.btc_client.call(cmd, args).await
	}
}

impl<F, P, N: Network> BlockManager<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Instantiates a new `BlockManager` instance.
	pub fn new(
		btc_client: BtcClient,
		bfc_client: Arc<EthClient<F, P, N>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		call_interval: u64,
		block_confirmations: u64,
		block_height_api: &'static str,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		let mut bootstrap_offset = 0u32;
		if let Some(bootstrap_config) = &bootstrap_shared_data.bootstrap_config {
			if bootstrap_config.is_enabled {
				bootstrap_offset = bootstrap_config
					.btc_block_offset
					.unwrap_or(DEFAULT_BITCOIN_BOOTSTRAP_BLOCK_OFFSET);
			}
		}

		Self {
			btc_client,
			bfc_client,
			sender,
			block_confirmations,
			waiting_block: Default::default(),
			processed_hashes: Default::default(),
			call_interval,
			bootstrap_shared_data,
			bootstrap_offset,
			block_height_api,
			head_monitor: HeadMonitor::new(
				Duration::from_millis(PROVIDER_STALE_BTC_HEAD_TIMEOUT_MS),
				BTC_HEAD_LAG_TOLERANCE,
			),
		}
	}

	/// Subscribe the event sender.
	pub fn subscribe(&self) -> Receiver<EventMessage> {
		self.sender.subscribe()
	}

	/// Bootstrap phase 0-1.
	pub async fn bootstrap_0(&mut self) -> Result<()> {
		self.initialize().await?;
		let should_bootstrap = self.is_before_bootstrap_state(BootstrapState::NormalStart).await;
		if should_bootstrap {
			self.wait_provider_sync().await?;
		}
		Ok(())
	}

	/// Initialize the block manager.
	pub async fn initialize(&mut self) -> Result<()> {
		self.btc_client.verify_fallbacks().await?;
		// bootstrap scans these blocks, so a reorg replacing any of them must be caught too
		let tracked = u64::from(self.bootstrap_offset).min(REORG_TRACK_DEPTH as u64 - 1);
		let (latest_block, blocks) = loop {
			let latest_block = self.get_block_count().await?;
			let tip = self.get_block_hash(latest_block).await?;
			let blocks = canonical_range(
				&BTreeMap::new(),
				latest_block.saturating_sub(tracked),
				latest_block,
				tip,
				1,
				|hash| self.header_link(hash),
			)
			.await?;
			if let Some(blocks) = blocks {
				break (latest_block, blocks);
			}
			log::warn!(
				target: LOG_TARGET,
				"-[{}] ⚠️  Provider served an unconfirmed block {} for #{:?}, retrying",
				sub_display_format(SUB_LOG_TARGET),
				tip,
				latest_block
			);
			sleep(Duration::from_millis(self.call_interval)).await;
		};
		self.waiting_block = latest_block.saturating_add(1);
		self.processed_hashes = blocks.into_iter().collect();

		log::info!(
			target: LOG_TARGET,
			"-[{}] 💤 Idle, best: #{:?}",
			sub_display_format(SUB_LOG_TARGET),
			latest_block
		);
		Ok(())
	}

	/// Wait for the provider to be synced.
	pub async fn wait_provider_sync(&self) -> Result<()> {
		let mut is_first_check = true;
		loop {
			let info = self.get_blockchain_info().await?;
			match info.initial_block_download {
				true => {
					if is_first_check {
						br_primitives::log_and_capture_simple!(
							warn,
							"⚙️  Syncing: #{:?}, Highest: #{:?} ({} relayer:{})",
							info.blocks,
							self.fetch_block_height().await,
							LOG_TARGET,
							self.bfc_client.address().await
						);
						is_first_check = false;
					}
					log::info!(
						target: LOG_TARGET,
						"-[{}] ⚙️  Syncing: #{:?}, Bitcoin is still in initial block download mode",
						sub_display_format(SUB_LOG_TARGET),
						info.blocks,
					);
				},
				false => {
					break;
				},
			}
			sleep(Duration::from_millis(self.bfc_client.metadata.call_interval)).await;
		}
		self.set_bootstrap_state(BootstrapState::BootstrapSocketRelay).await;
		log::info!(
			target: LOG_TARGET,
			"-[{}] ⚙️  [Bootstrap mode] NodeSyncing → BootstrapSocketRelay",
			sub_display_format(SUB_LOG_TARGET),
		);
		Ok(())
	}

	/// Fetch the block height from the offchain API.
	async fn fetch_block_height(&self) -> u64 {
		loop {
			match reqwest::get(self.block_height_api).await {
				Ok(response) => match response.json::<u64>().await {
					Ok(block_height) => {
						break block_height;
					},
					Err(e) => {
						log::warn!(
							target: LOG_TARGET,
							"-[{}] Failed to decode block height: {:?}. Retrying...",
							sub_display_format(SUB_LOG_TARGET),
							e
						);
						sleep(Duration::from_secs(5)).await;
					},
				},
				Err(e) => {
					log::warn!(
						target: LOG_TARGET,
						"-[{}] Failed to fetch block height: {:?}. Retrying...",
						sub_display_format(SUB_LOG_TARGET),
						e
					);
					sleep(Duration::from_secs(5)).await;
				},
			}
		}
	}

	/// Starts the block manager.
	pub async fn run(&mut self) -> Result<()> {
		let should_bootstrap = self.is_before_bootstrap_state(BootstrapState::NormalStart).await;
		if should_bootstrap {
			// Bootstrap failures are fatal (see `bootstrap_0`); only the live loop below retries.
			self.bootstrap().await.expect("bitcoin block manager bootstrap failed");
		}
		self.wait_for_all_chains_bootstrapped().await?;

		let mut stream = IntervalStream::new(interval(Duration::from_millis(self.call_interval)));
		while (stream.next().await).is_some() {
			let latest_block_num = self.get_block_count().await?;
			self.head_monitor.check(
				self.btc_client.failover(),
				latest_block_num,
				self.waiting_block.saturating_sub(1),
			);
			if self.is_block_confirmed(latest_block_num) {
				let (vault_set, refund_set) = self.fetch_registration_sets().await?;
				self.process_confirmed_block(
					latest_block_num.saturating_sub(self.block_confirmations),
					&vault_set,
					&refund_set,
				)
				.await?;
			}
		}
		Ok(())
	}

	/// Returns the generated user vault addresses.
	async fn get_vault_addresses(&self) -> Result<Vec<String>> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		let mut vault_addresses = registration_pool.vault_addresses(0).call().await?;
		vault_addresses
			.push(registration_pool.vault_address(*registration_pool.address(), 0).call().await?);

		Ok(vault_addresses)
	}

	/// Returns the registered user refund addresses.
	async fn get_refund_addresses(&self) -> Result<Vec<String>> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		Ok(registration_pool
			.refund_addresses(self.get_current_round().await?)
			.call()
			.await?)
	}

	/// Returns current pool round.
	async fn get_current_round(&self) -> Result<u32> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		Ok(registration_pool.current_round().call().await?)
	}

	/// Returns the vault and refund addresses.
	#[inline]
	async fn fetch_registration_sets(
		&self,
	) -> Result<(BTreeSet<Address<NetworkUnchecked>>, BTreeSet<Address<NetworkUnchecked>>)> {
		let vault_set: BTreeSet<Address<NetworkUnchecked>> = self
			.get_vault_addresses()
			.await?
			.iter()
			.map(|s| Address::from_str(s).unwrap())
			.collect();
		let refund_set: BTreeSet<Address<NetworkUnchecked>> = self
			.get_refund_addresses()
			.await?
			.iter()
			.map(|s| Address::from_str(s).unwrap())
			.collect();

		Ok((vault_set, refund_set))
	}

	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block_num: u64) -> bool {
		if self.waiting_block > latest_block_num {
			// difference is greater than 100 blocks
			// if it suddenly rollbacked to an old block
			if self.waiting_block > latest_block_num + 100 {
				br_primitives::log_and_capture_simple!(
					warn,
					"⚠️ [Bitcoin] Block rollbacked. From #{:?} to #{:?}",
					self.waiting_block,
					latest_block_num
				);
			}
			return false;
		}
		latest_block_num.saturating_sub(self.waiting_block) >= self.block_confirmations
	}

	/// Returns the confirmations (-1 off the canonical chain) and the parent of the block `hash`.
	async fn header_link(
		&self,
		hash: BlockHash,
	) -> bitcoincore_rpc::Result<(i32, Option<BlockHash>)> {
		let header = self.get_block_header_info(&hash).await?;
		Ok((header.confirmations, header.previous_block_hash))
	}

	/// Process the confirmed block. Filters whether the block has any Inbound or Outbound events.
	/// Blocks replaced by a reorg are scanned (and their events emitted) again. Re-emitted events
	/// are safe: handlers skip already-voted / already-executed requests, as they do for bootstrap.
	#[inline]
	async fn process_confirmed_block(
		&mut self,
		to_block: u64,
		vault_set: &BTreeSet<Address<NetworkUnchecked>>,
		refund_set: &BTreeSet<Address<NetworkUnchecked>>,
	) -> Result<()> {
		let from_block = self.waiting_block;

		let tip = self.get_block_hash(to_block).await?;
		let this = &*self;
		let blocks = canonical_range(
			&self.processed_hashes,
			from_block,
			to_block,
			tip,
			i32::try_from(self.block_confirmations).unwrap_or(i32::MAX),
			|hash| this.header_link(hash),
		)
		.await?;
		let Some(blocks) = blocks else {
			// retried on the next tick
			log::warn!(
				target: LOG_TARGET,
				"-[{}] ⚠️  Provider served an unconfirmed block {} for #{:?}, skipping",
				sub_display_format(SUB_LOG_TARGET),
				tip,
				to_block
			);
			return Ok(());
		};

		if let Some(&(rescan_from, _)) = blocks.first()
			&& rescan_from < from_block
		{
			self.processed_hashes.retain(|h, _| *h < rescan_from);
			self.waiting_block = rescan_from;
			br_primitives::log_and_capture_simple!(
				warn,
				"⚠️ [Bitcoin] Reorg detected at #{:?}. Rescanning from #{:?}",
				from_block,
				rescan_from
			);
		}

		for (num, block_hash) in blocks {
			let (mut inbound, mut outbound) =
				(EventMessage::inbound(num), EventMessage::outbound(num));

			let block = self.get_block_info_with_txs(&block_hash).await?;
			for tx in block.tx {
				self.filter(
					tx.txid,
					&tx.vout,
					&mut inbound.events,
					&mut outbound.events,
					vault_set,
					refund_set,
				)
				.await;
			}

			log::info!(
				target: LOG_TARGET,
				"-[{}] ✨ Imported #{:?} Inbound({:?}) Outbound({:?})",
				sub_display_format(SUB_LOG_TARGET),
				num,
				inbound.events.len(),
				outbound.events.len()
			);

			self.sender.send(EventMessage::new_block(num))?;
			self.sender.send(inbound)?;
			self.sender.send(outbound)?;

			// Advance per block so a failure on a later block doesn't re-emit the ones already
			// sent when the caller retries.
			self.processed_hashes.insert(num, block_hash);
			if self.processed_hashes.len() > REORG_TRACK_DEPTH {
				self.processed_hashes.pop_first();
			}
			self.increment_waiting_block(num);
		}

		Ok(())
	}

	/// Filter the transaction whether it contains Inbound or Outbound events.
	#[inline]
	async fn filter(
		&self,
		txid: Txid,
		vouts: &[GetRawTransactionResultVout],
		inbound_events: &mut Vec<Event>,
		outbound_events: &mut Vec<Event>,
		vault_set: &BTreeSet<Address<NetworkUnchecked>>,
		refund_set: &BTreeSet<Address<NetworkUnchecked>>,
	) {
		for vout in vouts {
			if let Some(address) = vout.script_pub_key.address.clone() {
				// address can only be contained in either one set.
				if vault_set.contains(&address) {
					inbound_events.push(Event { txid, index: vout.n, address, amount: vout.value });
					continue;
				}
				if refund_set.contains(&address) {
					outbound_events.push(Event {
						txid,
						index: vout.n,
						address,
						amount: vout.value,
					});
				}
			}
		}
	}

	/// Increment the current waiting block.
	#[inline]
	fn increment_waiting_block(&mut self, to: u64) {
		self.waiting_block = to.saturating_add(1);
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> BootstrapHandler for BlockManager<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn get_chain_id(&self) -> ChainId {
		self.bfc_client.get_bitcoin_chain_id().unwrap()
	}

	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		self.wait_for_bootstrap_state(BootstrapState::BootstrapSocketRelay).await?;

		let (inbound, outbound) = self.get_bootstrap_events().await?;

		self.sender.send(inbound)?;
		self.sender.send(outbound)?;

		self.set_bootstrap_state(BootstrapState::NormalStart).await;
		log::info!(
			target: LOG_TARGET,
			"-[{}] ⚙️  [Bootstrap mode] BootstrapSocketRelay → NormalStart",
			sub_display_format(SUB_LOG_TARGET),
		);
		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<(EventMessage, EventMessage)> {
		let (vault_set, refund_set) = self.fetch_registration_sets().await?;

		let to_block = self.waiting_block.saturating_sub(1);
		let from_block = to_block.saturating_sub(self.bootstrap_offset.into());

		let mut inbound = EventMessage::inbound(to_block);
		let mut outbound = EventMessage::outbound(to_block);

		// Walks parent links down from the tip `initialize` tracked, rather than looking each
		// height up: a hosted RPC can keep serving a replaced block's hash by height.
		let mut block_hash = match self.processed_hashes.get(&to_block) {
			Some(tracked) => *tracked,
			None => self.get_block_hash(to_block).await?,
		};
		let mut events = Vec::new();
		for _ in from_block..=to_block {
			let block = self.get_block_info_with_txs(&block_hash).await?;
			let (mut inbound, mut outbound) = (Vec::new(), Vec::new());
			for tx in block.tx {
				self.filter(
					tx.txid,
					&tx.vout,
					&mut inbound,
					&mut outbound,
					&vault_set,
					&refund_set,
				)
				.await;
			}
			events.push((inbound, outbound));
			match block.previousblockhash {
				Some(parent) => block_hash = parent,
				None => break,
			}
		}
		// lowest block first, as scanned before
		for (inbound_events, outbound_events) in events.into_iter().rev() {
			inbound.events.extend(inbound_events);
			outbound.events.extend(outbound_events);
		}
		Ok((inbound, outbound))
	}
}

/// Returns the `(height, hash)` of every block to scan, ascending: the canonical chain from
/// `from` to `to`, preceded by the tracked blocks below `from` that a reorg replaced (down to
/// the end of the tracked window at most).
/// Found by walking parent links down from `tip`, the hash at `to`: a hosted RPC can keep
/// serving a replaced block's hash by height, so only `tip` is looked up that way, and `None` is
/// returned if it has under `min_confirmations` (a replaced block reports -1).
async fn canonical_range<E, Fut>(
	tracked: &BTreeMap<u64, BlockHash>,
	from: u64,
	to: u64,
	tip: BlockHash,
	min_confirmations: i32,
	mut header: impl FnMut(BlockHash) -> Fut,
) -> Result<Option<Vec<(u64, BlockHash)>>, E>
where
	Fut: Future<Output = Result<(i32, Option<BlockHash>), E>>,
{
	let (mut height, mut hash) = (to, tip);
	let mut blocks = Vec::new();
	loop {
		// below `from`: stop at the first block still tracked as is, or out of the window
		if height < from && tracked.get(&height).is_none_or(|tracked| *tracked == hash) {
			break;
		}
		let (confirmations, parent) = header(hash).await?;
		if height == to && confirmations < min_confirmations {
			return Ok(None);
		}
		blocks.push((height, hash));
		match (height.checked_sub(1), parent) {
			(Some(below), Some(parent)) => (height, hash) = (below, parent),
			_ => break,
		}
	}
	blocks.reverse();
	Ok(Some(blocks))
}

#[cfg(test)]
mod tests {
	use super::*;
	use miniscript::bitcoin::hashes::Hash;

	fn hash(n: u8) -> BlockHash {
		BlockHash::from_byte_array([n; 32])
	}

	/// Scans up to #13 from `from`, on a node whose chain is `canonical` (ascending, ending at
	/// #13) and whose tip has `confirmations`.
	async fn range(
		tracked: &[(u64, u8)],
		from: u64,
		canonical: &[u8],
		confirmations: i32,
	) -> Option<Vec<(u64, u8)>> {
		let tracked: BTreeMap<_, _> = tracked.iter().map(|&(h, n)| (h, hash(n))).collect();
		let parent_of = |h: BlockHash| {
			let at = canonical.iter().position(|&n| hash(n) == h).unwrap();
			at.checked_sub(1).map(|below| hash(canonical[below]))
		};
		let tip = hash(*canonical.last().unwrap());
		canonical_range::<(), _>(&tracked, from, 13, tip, 3, |h| {
			std::future::ready(Ok((confirmations, parent_of(h))))
		})
		.await
		.unwrap()
		.map(|blocks| blocks.into_iter().map(|(h, hash)| (h, hash.to_byte_array()[0])).collect())
	}

	#[tokio::test]
	async fn scans_only_new_blocks_without_a_reorg() {
		let tracked = [(10, 1), (11, 2)];
		assert_eq!(range(&tracked, 12, &[1, 2, 3, 4], 4).await, Some(vec![(12, 3), (13, 4)]));
	}

	#[tokio::test]
	async fn rescans_above_highest_block_still_canonical() {
		let tracked = [(10, 1), (11, 2), (12, 3)];
		assert_eq!(
			range(&tracked, 13, &[1, 7, 8, 9], 4).await,
			Some(vec![(11, 7), (12, 8), (13, 9)])
		);
	}

	#[tokio::test]
	async fn rescans_whole_window_when_reorg_is_deeper_than_tracked() {
		let tracked = [(11, 2), (12, 3)];
		assert_eq!(
			range(&tracked, 13, &[6, 7, 8, 9], 4).await,
			Some(vec![(11, 7), (12, 8), (13, 9)])
		);
	}

	#[tokio::test]
	async fn skips_a_tip_off_the_canonical_chain() {
		assert_eq!(range(&[(12, 3)], 13, &[1, 2, 3, 4], -1).await, None);
	}
}
