use std::sync::Arc;

use ethers::{
	providers::{JsonRpcClient, Middleware},
	types::{BlockNumber, Filter, Log, SyncingStatus, H256, U64},
};
use tokio::{
	sync::{
		broadcast::{self, Receiver, Sender},
		RwLock,
	},
	time::{sleep, Duration},
};

use br_primitives::{
	eth::{BootstrapState, ChainID},
	sub_display_format,
};

use super::{BootstrapHandler, EthClient};

#[derive(Clone, Debug)]
/// The message format passed through the block channel.
pub struct BlockMessage {
	/// The processed block number.
	pub block_number: U64,
	/// The processed block hash.
	pub block_hash: H256,
	/// The detected transaction logs from the target contracts.
	pub target_logs: Vec<Log>,
}

impl BlockMessage {
	pub fn new(block_number: U64, block_hash: H256, target_logs: Vec<Log>) -> Self {
		Self { block_number, block_hash, target_logs }
	}
}

/// The message receiver connected to the block channel.
pub struct BlockReceiver {
	/// The chain ID of the block channel.
	pub id: ChainID,
	/// The message receiver.
	pub receiver: Receiver<BlockMessage>,
}

impl BlockReceiver {
	pub fn new(id: ChainID, receiver: Receiver<BlockMessage>) -> Self {
		Self { id, receiver }
	}
}

const SUB_LOG_TARGET: &str = "block-manager";

/// The essential task that listens and handle new blocks.
pub struct BlockManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The channel sending block messages.
	pub sender: Sender<BlockMessage>,
	/// The block waiting for enough confirmations.
	pub waiting_block: U64,
	/// State of bootstrapping
	pub bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
}

impl<T: JsonRpcClient> BlockManager<T> {
	/// Instantiates a new `BlockManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self { client, sender, waiting_block: U64::default(), bootstrap_states }
	}

	/// Initialize block manager.
	async fn initialize(&mut self) {
		// initialize waiting block to the latest block
		self.waiting_block = self.client.get_latest_block_number().await;
		if let Some(block) = self.client.get_block(self.waiting_block.into()).await {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 💤 Idle, best: #{:?} ({})",
				sub_display_format(SUB_LOG_TARGET),
				block.number.unwrap(),
				block.hash.unwrap(),
			);
		}

		self.client.sync_balance().await;
	}

	/// Starts the block manager. Reads every new mined block of the connected chain and starts to
	/// publish to the block channel.
	pub async fn run(&mut self) {
		self.initialize().await;

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block = self.client.get_latest_block_number().await;
				while self.is_block_confirmed(latest_block) {
					self.process_confirmed_block().await;
					self.increment_waiting_block();

					self.client.sync_balance().await;
				}
			}

			sleep(Duration::from_millis(self.client.call_interval)).await;
		}
	}

	/// Process the confirmed block and verifies if any transaction interacted with the target
	/// contracts.
	async fn process_confirmed_block(&self) {
		if let Some(block) = self.client.get_block(self.waiting_block.into()).await {
			let filter = Filter::new()
				.from_block(BlockNumber::from(self.waiting_block))
				.to_block(BlockNumber::from(self.waiting_block))
				.address(self.client.socket.address());

			let target_logs = self.client.get_logs(&filter).await;
			if !target_logs.is_empty() {
				self.sender
					.send(BlockMessage::new(
						block.number.unwrap(),
						block.hash.unwrap(),
						target_logs,
					))
					.unwrap();
			}

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ✨ Imported #{:?} ({})",
				sub_display_format(SUB_LOG_TARGET),
				block.number.unwrap(),
				block.hash.unwrap(),
			);
		}
	}

	/// Increment the waiting block.
	fn increment_waiting_block(&mut self) {
		self.waiting_block = self.waiting_block.saturating_add(U64::from(1u64));
		br_metrics::set_block_height(&self.client.get_chain_name(), self.waiting_block.as_u64());
	}

	/// Verifies if the stored waiting block has waited enough.
	fn is_block_confirmed(&self, latest_block: U64) -> bool {
		latest_block.saturating_sub(self.waiting_block) > self.client.block_confirmations
	}

	/// Verifies if the connected provider is in block sync mode.
	pub async fn wait_provider_sync(&self) {
		if let Err(error) = self.client.provider.client_version().await {
			panic!(
				"[{}]-[{}]-[{}] An internal error thrown when making a call to the provider. Please check your provider's status [method: client_version]: {}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address(),
				error
			);
		}

		loop {
			if let SyncingStatus::IsSyncing(status) = self.client.is_syncing().await {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚙️  Syncing #{:?}, Highest: #{:?}",
					sub_display_format(SUB_LOG_TARGET),
					status.current_block,
					status.highest_block,
				);
			} else {
				for state in self.bootstrap_states.write().await.iter_mut() {
					if *state == BootstrapState::NodeSyncing {
						*state = BootstrapState::BootstrapRoundUpPhase1;
					}
				}
				return
			}

			sleep(Duration::from_millis(self.client.call_interval)).await;
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for BlockManager<T> {
	async fn bootstrap(&self) {}

	async fn get_bootstrap_events(&self) -> Vec<Log> {
		vec![]
	}

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_states.read().await.iter().all(|s| *s == state)
	}
}
