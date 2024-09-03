use std::sync::Arc;

use ethers::{
	providers::JsonRpcClient,
	types::{BlockNumber, Filter, Log, SyncingStatus, U64},
};
use subxt::tx::Signer;
use tokio::{
	sync::broadcast::{self, Receiver, Sender},
	time::{sleep, Duration},
};

use super::{traits::BootstrapHandler, EthClient};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	eth::{BootstrapState, ChainID},
	substrate::CustomConfig,
	utils::sub_display_format,
};

#[derive(Clone, Debug)]
/// The message format passed through the block channel.
pub struct EventMessage {
	/// The processed block number.
	pub block_number: U64,
	/// The detected transaction logs from the target contracts.
	pub event_logs: Vec<Log>,
}

impl EventMessage {
	pub fn new(block_number: U64, event_logs: Vec<Log>) -> Self {
		Self { block_number, event_logs }
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

const SUB_LOG_TARGET: &str = "event-manager";

/// The essential task that listens and handle new events.
pub struct EventManager<T, S> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T, S>>,
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

impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig> + Send + Sync> EventManager<T, S> {
	/// Instantiates a new `EventManager` instance.
	pub fn new(
		client: Arc<EthClient<T, S>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		is_balance_sync_enabled: bool,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self {
			client,
			sender,
			waiting_block: U64::default(),
			bootstrap_shared_data,
			is_balance_sync_enabled,
		}
	}

	/// Initialize event manager.
	async fn initialize(&mut self) {
		self.client.verify_chain_id().await;
		self.client.verify_minimum_balance().await;

		// initialize waiting block to the latest block + 1
		let latest_block = self.client.get_latest_block_number().await;
		self.waiting_block = latest_block.saturating_add(U64::from(1u64));
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 💤 Idle, best: #{:?}",
			sub_display_format(SUB_LOG_TARGET),
			latest_block
		);

		if self.is_balance_sync_enabled {
			self.client.sync_balance().await;
		}
	}

	/// Starts the event manager. Reads every new mined block of the connected chain and starts to
	/// publish to the event channel.
	pub async fn run(&mut self) {
		self.initialize().await;

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block = self.client.get_latest_block_number().await;
				while self.is_block_confirmed(latest_block) {
					self.process_confirmed_block().await;

					if self.is_balance_sync_enabled {
						self.client.sync_balance().await;
					}
				}
			}

			sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
		}
	}

	/// Process the confirmed block and verifies if any events emitted from the target
	/// contracts.
	async fn process_confirmed_block(&mut self) {
		let from = self.waiting_block;
		let to = from.saturating_add(
			self.client.metadata.get_logs_batch_size.saturating_sub(U64::from(1u64)),
		);

		let filter = if let Some(bitcoin_socket) = &self.client.protocol_contracts.bitcoin_socket {
			Filter::new()
				.from_block(BlockNumber::from(from))
				.to_block(BlockNumber::from(to))
				.address(vec![
					self.client.protocol_contracts.socket.address(),
					bitcoin_socket.address(),
				])
		} else {
			Filter::new()
				.from_block(BlockNumber::from(from))
				.to_block(BlockNumber::from(to))
				.address(self.client.protocol_contracts.socket.address())
		};

		let target_logs = self.client.get_logs(&filter).await;
		if !target_logs.is_empty() {
			self.sender.send(EventMessage::new(self.waiting_block, target_logs)).unwrap();
		}

		if from < to {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ✨ Imported #({:?} … {:?})",
				sub_display_format(SUB_LOG_TARGET),
				from,
				to
			);
		} else {
			log::info!(
				target: &self.client.get_chain_name(),
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
		br_metrics::set_block_height(&self.client.get_chain_name(), self.waiting_block.as_u64());
	}

	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block: U64) -> bool {
		if self.waiting_block > latest_block {
			return false;
		}
		latest_block.saturating_sub(self.waiting_block) >= self.client.metadata.block_confirmations
	}

	/// Verifies if the connected provider is in block sync mode.
	pub async fn wait_provider_sync(&self) {
		loop {
			match self.client.is_syncing().await {
				SyncingStatus::IsFalse => {
					for state in
						self.bootstrap_shared_data.bootstrap_states.write().await.iter_mut()
					{
						if *state == BootstrapState::NodeSyncing {
							*state = BootstrapState::BootstrapRoundUpPhase1;
						}
					}
					return;
				},
				SyncingStatus::IsSyncing(status) => {
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚙️  Syncing: #{:?}, Highest: #{:?}",
						sub_display_format(SUB_LOG_TARGET),
						status.current_block,
						status.highest_block,
					);
				},
				SyncingStatus::IsArbitrumSyncing(status) => {
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚙️  Processed batch: #{:?}, Last batch: #{:?}",
						sub_display_format(SUB_LOG_TARGET),
						status.batch_processed,
						status.batch_seen
					)
				},
			}

			sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient, S: Signer<CustomConfig>> BootstrapHandler for EventManager<T, S>
where
	S: Sync,
	S: Send,
{
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) {}

	async fn get_bootstrap_events(&self) -> Vec<Log> {
		vec![]
	}
}
