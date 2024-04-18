use crate::{
	btc::{storage::pending_outbound::PendingOutboundPool, LOG_TARGET},
	eth::EthClient,
};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET,
		errors::PROVIDER_INTERNAL_ERROR,
		tx::{DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS},
	},
	eth::BootstrapState,
	utils::sub_display_format,
};

use bitcoincore_rpc::{
	bitcoincore_rpc_json::GetRawTransactionResultVout, Client as BtcClient, RpcApi,
};
use ethers::providers::JsonRpcClient;
use miniscript::bitcoin::{address::NetworkUnchecked, Address, Amount, Txid};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, str::FromStr, sync::Arc};
use tokio::{
	sync::{
		broadcast,
		broadcast::{Receiver, Sender},
	},
	time::{sleep, Duration},
};
use tokio_stream::StreamExt;

use super::handlers::BootstrapHandler;

const SUB_LOG_TARGET: &str = "block-manager";

#[derive(Debug, Clone, Eq, PartialEq)]
/// A Bitcoin related event type.
pub enum EventType {
	/// An inbound action.
	Inbound,
	/// An outbound action.
	Outbound,
	/// A new block mined.
	NewBlock,
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

	/// Instantiates an `NewBlock` typed `EventMessage` instance.
	pub fn new_block(block_number: u64) -> Self {
		Self::new(block_number, EventType::NewBlock, vec![])
	}
}

/// A module that reads every new Bitcoin block and filters `Inbound`, `Outbound` events.
pub struct BlockManager<T> {
	/// The Bitcoin client.
	btc_client: BtcClient,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T>>,
	/// The event message sender.
	sender: Sender<EventMessage>,
	/// The configured minimum block confirmations required to process a block.
	block_confirmations: u64,
	/// The block that is waiting for confirmations.
	waiting_block: u64,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// TODO: need usecase
	_pending_outbounds: PendingOutboundPool,
}

#[async_trait::async_trait]
impl<C: JsonRpcClient> RpcApi for BlockManager<C> {
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

impl<T: JsonRpcClient + 'static> BlockManager<T> {
	/// Instantiates a new `BlockManager` instance.
	pub fn new(
		btc_client: BtcClient,
		bfc_client: Arc<EthClient<T>>,
		_pending_outbounds: PendingOutboundPool,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self {
			btc_client,
			bfc_client,
			sender,
			block_confirmations: 0,
			waiting_block: 0,
			bootstrap_shared_data,
			_pending_outbounds,
		}
	}

	/// Subscribe the event sender.
	pub fn subscribe(&self) -> Receiver<EventMessage> {
		self.sender.subscribe()
	}

	/// Starts the block manager.
	pub async fn run(&mut self) {
		self.waiting_block = self.get_block_count().await.unwrap();

		log::info!(
			target: LOG_TARGET,
			"-[{}] 💤 Idle, best: #{:?}",
			sub_display_format(SUB_LOG_TARGET),
			self.waiting_block
		);

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapSocketRelay).await {
				self.bootstrap().await;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block_num = self.get_block_count().await.unwrap();
				if self.is_block_confirmed(latest_block_num) {
					let (vault_set, refund_set) = self.fetch_registration_sets().await;
					self.process_confirmed_block(latest_block_num, &vault_set, &refund_set).await;
				}
			}

			self.wait_for_new_block(0).await.unwrap();
		}
	}

	/// Returns the generated user vault addresses.
	async fn get_vault_addresses(&self) -> Vec<String> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		self.bfc_client
			.contract_call(registration_pool.vault_addresses(), "registration_pool.vault_addresses")
			.await
	}

	/// Returns the registered user refund addresses.
	async fn get_refund_addresses(&self) -> Vec<String> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		self.bfc_client
			.contract_call(
				registration_pool.refund_addresses(),
				"registration_pool.refund_addresses",
			)
			.await
	}

	/// Returns the vault and refund addresses.
	#[inline]
	async fn fetch_registration_sets(
		&self,
	) -> (BTreeSet<Address<NetworkUnchecked>>, BTreeSet<Address<NetworkUnchecked>>) {
		let vault_set: BTreeSet<Address<NetworkUnchecked>> = self
			.get_vault_addresses()
			.await
			.iter()
			.map(|s| Address::from_str(s).unwrap())
			.collect();
		let refund_set: BTreeSet<Address<NetworkUnchecked>> = self
			.get_refund_addresses()
			.await
			.iter()
			.map(|s| Address::from_str(s).unwrap())
			.collect();

		(vault_set, refund_set)
	}

	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block_num: u64) -> bool {
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
			let (mut inbound, mut outbound, new_block) = (
				EventMessage::inbound(num),
				EventMessage::outbound(num),
				EventMessage::new_block(num),
			);

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
			self.sender.send(new_block).unwrap();
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
				if vault_set.contains(&address) {
					inbound_events.push(Event {
						txid,
						index: vout.n,
						address: address.clone(),
						amount: vout.value,
					});
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
impl<T: JsonRpcClient + 'static> BootstrapHandler for BlockManager<T> {
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) {
		log::info!(
			target: LOG_TARGET,
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping Bitcoin events",
			sub_display_format(SUB_LOG_TARGET),
		);

		let (inbound, outbound) = self.get_bootstrap_events().await;

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
	}

	async fn get_bootstrap_events(&self) -> (EventMessage, EventMessage) {
		let (vault_set, refund_set) = self.fetch_registration_sets().await;
		let to_block = self.waiting_block.saturating_sub(1);

		let mut inbound = EventMessage::inbound(to_block);
		let mut outbound = EventMessage::outbound(to_block);

		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			let round_info = self
				.bfc_client
				.contract_call(
					self.bfc_client.protocol_contracts.authority.round_info(),
					"authority.round_info",
				)
				.await;

			let bootstrap_offset_height = self.get_bootstrap_offset_height_based_on_block_time(
				bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
				round_info,
			);

			let from_block = to_block.saturating_sub(bootstrap_offset_height.into());
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
		}
		(inbound, outbound)
	}
}