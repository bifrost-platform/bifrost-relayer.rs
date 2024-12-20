use crate::{btc::LOG_TARGET, eth::EthClient};

use alloy::{
	network::AnyNetwork,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	transports::Transport,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BITCOIN_BOOTSTRAP_BLOCK_OFFSET,
		errors::PROVIDER_INTERNAL_ERROR,
		tx::{DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS},
	},
	eth::BootstrapState,
	utils::sub_display_format,
};
use eyre::Result;

use bitcoincore_rpc::{
	bitcoincore_rpc_json::GetRawTransactionResultVout, Client as BtcClient, RpcApi,
};
use miniscript::bitcoin::{address::NetworkUnchecked, Address, Amount, Txid};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, str::FromStr, sync::Arc};
use tokio::{
	sync::broadcast::{self, Receiver, Sender},
	time::{interval, sleep, Duration},
};
use tokio_stream::{wrappers::IntervalStream, StreamExt};

use super::handlers::BootstrapHandler;

const SUB_LOG_TARGET: &str = "block-manager";

#[derive(Debug, Clone, Eq, PartialEq)]
/// A Bitcoin related event type.
pub enum EventType {
	/// An inbound action.
	Inbound,
	/// An outbound action.
	Outbound,
}

#[derive(Debug, Clone)]
/// A Bitcoin related event details. (Only for `Inbound` and `Outbound`)
pub struct Event {
	/// The transaction hash.
	pub txid: Txid,
	/// The output index of the transaction.
	pub index: u32,
	/// The account address.
	pub address: Address<NetworkUnchecked>,
	/// The transferred amount.
	pub amount: Amount,
}

#[derive(Debug, Clone)]
/// The event message delivered through channels.
pub struct EventMessage {
	/// The current block number.
	pub block_number: u64,
	/// The event type.
	pub event_type: EventType,
	/// The event details.
	pub events: Vec<Event>,
}

impl EventMessage {
	/// Instantiates a new `EventMessage` instance.
	pub fn new(block_number: u64, event_type: EventType, events: Vec<Event>) -> Self {
		Self { block_number, event_type, events }
	}

	/// Instantiates an `Inbound` typed `EventMessage` instance.
	pub fn inbound(block_number: u64) -> Self {
		Self::new(block_number, EventType::Inbound, vec![])
	}

	/// Instantiates an `Outbound` typed `EventMessage` instance.
	pub fn outbound(block_number: u64) -> Self {
		Self::new(block_number, EventType::Outbound, vec![])
	}
}

/// A module that reads every new Bitcoin block and filters `Inbound`, `Outbound` events.
pub struct BlockManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The Bitcoin client.
	btc_client: BtcClient,
	/// The Bifrost client.
	pub bfc_client: Arc<EthClient<F, P, T>>,
	/// The event message sender.
	sender: Sender<EventMessage>,
	/// The configured minimum block confirmations required to process a block.
	block_confirmations: u64,
	/// The block that is waiting for confirmations.
	waiting_block: u64,
	/// The `getblockcount` request interval in milliseconds.
	call_interval: u64,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The bootstrap offset in blocks.
	bootstrap_offset: u32,
}

#[async_trait::async_trait]
impl<F, P, TR> RpcApi for BlockManager<F, P, TR>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<TR, AnyNetwork>,
	TR: Transport + Clone,
{
	async fn call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> bitcoincore_rpc::Result<T> {
		let mut error_msg = String::default();
		for _ in 0..DEFAULT_CALL_RETRIES {
			match self.btc_client.call(cmd, args).await {
				Ok(ret) => return Ok(ret),
				Err(e) => {
					error_msg = e.to_string();
				},
			}
			sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
		}
		panic!(
			"[{}]-[{}] {} [cmd: {}]: {}",
			LOG_TARGET,
			crate::btc::SUB_LOG_TARGET,
			PROVIDER_INTERNAL_ERROR,
			cmd,
			error_msg
		);
	}
}

impl<F, P, T> BlockManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Instantiates a new `BlockManager` instance.
	pub fn new(
		btc_client: BtcClient,
		bfc_client: Arc<EthClient<F, P, T>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		call_interval: u64,
		block_confirmations: u64,
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
			call_interval,
			bootstrap_shared_data,
			bootstrap_offset,
		}
	}

	/// Subscribe the event sender.
	pub fn subscribe(&self) -> Receiver<EventMessage> {
		self.sender.subscribe()
	}

	/// Starts the block manager.
	pub async fn run(&mut self) -> Result<()> {
		let latest_block = self.get_block_count().await.unwrap();
		self.waiting_block = latest_block.saturating_add(1);

		log::info!(
			target: LOG_TARGET,
			"-[{}] 💤 Idle, best: #{:?}",
			sub_display_format(SUB_LOG_TARGET),
			latest_block
		);

		let mut stream = IntervalStream::new(interval(Duration::from_millis(self.call_interval)));
		while let Some(_) = stream.next().await {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapSocketRelay).await {
				self.bootstrap().await?;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block_num = self.get_block_count().await.unwrap();
				if self.is_block_confirmed(latest_block_num) {
					let (vault_set, refund_set) = self.fetch_registration_sets().await?;
					self.process_confirmed_block(
						latest_block_num.saturating_sub(self.block_confirmations),
						&vault_set,
						&refund_set,
					)
					.await;
				}
			}
		}
		Ok(())
	}

	/// Returns the generated user vault addresses.
	async fn get_vault_addresses(&self) -> Result<Vec<String>> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		Ok(registration_pool
			.vault_addresses(self.get_current_round().await?)
			.call()
			.await?
			._0)
	}

	/// Returns the registered user refund addresses.
	async fn get_refund_addresses(&self) -> Result<Vec<String>> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		Ok(registration_pool
			.refund_addresses(self.get_current_round().await?)
			.call()
			.await?
			._0)
	}

	/// Returns current pool round.
	async fn get_current_round(&self) -> Result<u32> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		Ok(registration_pool.current_round().call().await?._0)
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
			return false;
		}
		latest_block_num.saturating_sub(self.waiting_block) >= self.block_confirmations
	}

	/// Process the confirmed block. Filters whether the block has any Inbound or Outbound events.
	#[inline]
	async fn process_confirmed_block(
		&mut self,
		to_block: u64,
		vault_set: &BTreeSet<Address<NetworkUnchecked>>,
		refund_set: &BTreeSet<Address<NetworkUnchecked>>,
	) {
		let from_block = self.waiting_block;

		for num in from_block..=to_block {
			let (mut inbound, mut outbound) =
				(EventMessage::inbound(num), EventMessage::outbound(num));

			let block_hash = self.get_block_hash(num).await.unwrap();
			let txs = self.get_block_info_with_txs(&block_hash).await.unwrap().tx;

			let mut stream = tokio_stream::iter(txs.iter());
			while let Some(tx) = stream.next().await {
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

			self.sender.send(inbound).unwrap();
			self.sender.send(outbound).unwrap();
		}

		self.increment_waiting_block(to_block);
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
		let mut stream = tokio_stream::iter(vouts.iter());
		while let Some(vout) = stream.next().await {
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
impl<F, P, T> BootstrapHandler for BlockManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		log::info!(
			target: LOG_TARGET,
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping Bitcoin events",
			sub_display_format(SUB_LOG_TARGET),
		);

		let (inbound, outbound) = self.get_bootstrap_events().await?;

		self.sender.send(inbound).unwrap();
		self.sender.send(outbound).unwrap();

		let mut bootstrap_count = self.bootstrap_shared_data.socket_bootstrap_count.lock().await;
		*bootstrap_count += 1;

		if *bootstrap_count == self.bootstrap_shared_data.system_providers_len as u8 {
			let mut bootstrap_guard = self.bootstrap_shared_data.bootstrap_states.write().await;

			for state in bootstrap_guard.iter_mut() {
				*state = BootstrapState::NormalStart;
			}

			log::info!(
				target: "bifrost-relayer",
				"-[{}] ⚙️  [Bootstrap mode] Bootstrap process successfully ended.",
				sub_display_format(SUB_LOG_TARGET),
			);
		}

		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<(EventMessage, EventMessage)> {
		let (vault_set, refund_set) = self.fetch_registration_sets().await?;

		let to_block = self.waiting_block.saturating_sub(1);
		let from_block = to_block.saturating_sub(self.bootstrap_offset.into());

		let mut inbound = EventMessage::inbound(to_block);
		let mut outbound = EventMessage::outbound(to_block);

		for i in from_block..=to_block {
			let block_hash = self.get_block_hash(i).await.unwrap();
			let txs = self.get_block_info_with_txs(&block_hash).await.unwrap().tx;
			let mut stream = tokio_stream::iter(txs);

			while let Some(tx) = stream.next().await {
				self.filter(
					tx.txid,
					&tx.vout,
					&mut inbound.events,
					&mut outbound.events,
					&vault_set,
					&refund_set,
				)
				.await;
			}
		}
		Ok((inbound, outbound))
	}
}
