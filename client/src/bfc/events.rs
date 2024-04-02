use super::{CustomConfig, SubClient};
use subxt::events::EventDetails;

use std::sync::Arc;

use ethers::{
	providers::JsonRpcClient,
	types::{BlockNumber, Filter, Log, SyncingStatus, U64},
};
use tokio::{
	sync::broadcast::{self, Receiver, Sender},
	time::{sleep, Duration},
};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	eth::{BootstrapState, ChainID},
	sub_display_format,
};

use crate::eth::traits::BootstrapHandler;

const SUB_LOG_TARGET: &str = "event-manager";

#[derive(Debug, Clone)]
/// The message format passed through the block channel.
pub struct EventMessage {
	/// The processed block number.
	pub block_number: U64,
	/// The detected transaction logs from the target contracts.
	pub events: Vec<EventDetails<CustomConfig>>,
}

impl EventMessage {
	pub fn new(block_number: U64, events: Vec<EventDetails<CustomConfig>>) -> Self {
		Self { block_number, events }
	}
}

/// The message receiver connected to the block channel.
pub struct EventReceiver {
	/// The chain ID of the block channel.
	pub id: ChainID,
	/// The message receiver.
	pub receiver: Receiver<EventMessage>,
}

impl EventReceiver {
	pub fn new(id: ChainID, receiver: Receiver<EventMessage>) -> Self {
		Self { id, receiver }
	}
}

/// The essential task that listens and handle new events.
pub struct EventManager<T> {
	/// The ethereum client for the connected chain.
	pub sub_client: Arc<SubClient<T>>,
	/// The channel sending event messages.
	pub sender: Sender<EventMessage>,
	/// The block waiting for enough confirmations.
	waiting_block: U64,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The flag whether the relayer has enabled self balance synchronization. This field will be
	/// enabled when prometheus exporter is enabled.
	is_balance_sync_enabled: bool,
}

impl<T: JsonRpcClient> EventManager<T> {
	pub fn new(
		sub_client: Arc<SubClient<T>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		is_balance_sync_enabled: bool,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self {
			sub_client,
			sender,
			waiting_block: U64::default(),
			bootstrap_shared_data,
			is_balance_sync_enabled,
		}
	}

	/// Initialize event manager.
	async fn initialize(&mut self) {
		self.sub_client.eth_client.verify_chain_id().await;
		self.sub_client.eth_client.verify_minimum_balance().await;

		// initialize waiting block to the latest block
		self.waiting_block = self.sub_client.eth_client.get_latest_block_number().await;
		log::info!(
			target: &self.sub_client.eth_client.get_chain_name(),
			"-[{}] 💤 Idle, best: #{:?}",
			sub_display_format(SUB_LOG_TARGET),
			self.waiting_block
		);

		if self.is_balance_sync_enabled {
			self.sub_client.eth_client.sync_balance().await;
		}
	}

	/// Starts the event manager. Reads every new mined block of the connected chain and starts to
	/// publish to the event channel.
	pub async fn run(&mut self) {
		self.initialize().await;

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block = self.sub_client.eth_client.get_latest_block_number().await;
				while self.is_block_confirmed(latest_block) {
					self.process_confirmed_block().await;

					if self.is_balance_sync_enabled {
						self.sub_client.eth_client.sync_balance().await;
					}
				}
			}

			sleep(Duration::from_millis(self.sub_client.eth_client.metadata.call_interval)).await;
		}
	}

	/// Process the confirmed block and verifies if any events emitted from the target
	/// contracts.
	async fn process_confirmed_block(&mut self) {
		let mut from = self.waiting_block;
		let to: U64 = from.saturating_add(
			self.sub_client
				.eth_client
				.metadata
				.get_logs_batch_size
				.saturating_sub(U64::from(1u64)),
		);

		let target_events = self.sub_client.filter_block_event(from, to).await;
		if !target_events.is_empty() {
			self.sender.send(EventMessage::new(self.waiting_block, target_events)).unwrap();
		}

		if from < to {
			log::info!(
				target: &self.sub_client.eth_client.get_chain_name(),
				"-[{}] ✨ Imported #({:?} … {:?})",
				sub_display_format(SUB_LOG_TARGET),
				from,
				to
			);
		} else {
			log::info!(
				target: &self.sub_client.eth_client.get_chain_name(),
				"-[{}] ✨ Imported #{:?}",
				sub_display_format(SUB_LOG_TARGET),
				self.waiting_block
			);
		}

		self.increment_waiting_block(to);
	}

	/// Increment the waiting block.
	fn increment_waiting_block(&mut self, to: U64) {
		self.waiting_block = to.saturating_add(U64::from(1u64));
		br_metrics::set_block_height(
			&self.sub_client.eth_client.get_chain_name(),
			self.waiting_block.as_u64(),
		);
	}

	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block: U64) -> bool {
		latest_block.saturating_sub(self.waiting_block)
			>= self.sub_client.eth_client.metadata.block_confirmations
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for EventManager<T> {
	async fn bootstrap(&self) {}

	async fn get_bootstrap_events(&self) -> Vec<Log> {
		vec![]
	}

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}
