use cccp_primitives::{
	eth::{BootstrapState, ChainID},
	sub_display_format,
};
use ethers::{
	providers::JsonRpcClient,
	types::{Log, SyncingStatus, TransactionReceipt, H160, H256, U64},
};
use std::sync::Arc;
use tokio::{
	sync::{
		broadcast::{self, Receiver, Sender},
		RwLock,
	},
	time::{sleep, Duration},
};
use tokio_stream::StreamExt;

use super::{BootstrapHandler, EthClient};

#[derive(Clone, Debug)]
/// The message format passed through the block channel.
pub struct BlockMessage {
	/// The information of the processed block.
	pub block_number: U64,
	pub block_hash: H256,
	/// The transaction receipts from the target contracts.
	pub target_receipts: Vec<TransactionReceipt>,
}

impl BlockMessage {
	pub fn new(
		block_number: U64,
		block_hash: H256,
		target_receipts: Vec<TransactionReceipt>,
	) -> Self {
		Self { block_number, block_hash, target_receipts }
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
	/// The target contracts this chain is watching.
	pub target_contracts: Vec<H160>,
	/// The block waiting for enough confirmations.
	pub waiting_block: U64,
	/// State of bootstrapping
	pub bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
}

impl<T: JsonRpcClient> BlockManager<T> {
	/// Instantiates a new `BlockManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		target_contracts: Vec<H160>,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self { client, sender, target_contracts, waiting_block: U64::default(), bootstrap_states }
	}

	/// Initialize block manager.
	async fn initialize(&mut self) {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 📃 Target contracts: {:?}",
			sub_display_format(SUB_LOG_TARGET),
			self.target_contracts
		);

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
				}
			}

			sleep(Duration::from_millis(self.client.call_interval)).await;
		}
	}

	/// Process the confirmed block and verifies if any transaction interacted with the target
	/// contracts.
	async fn process_confirmed_block(&self) {
		if let Some(block) = self.client.get_block_with_txs(self.waiting_block.into()).await {
			let mut target_receipts = vec![];
			let mut stream = tokio_stream::iter(block.clone().transactions);

			while let Some(tx) = stream.next().await {
				if self.is_relay_target(tx.to) {
					if let Some(receipt) = self.client.get_transaction_receipt(tx.hash).await {
						target_receipts.push(receipt);
					}
				}
			}
			if !target_receipts.is_empty() {
				self.sender
					.send(BlockMessage::new(
						block.number.unwrap(),
						block.hash.unwrap(),
						target_receipts,
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
	}

	/// Verifies if the transaction has interacted with the target contracts.
	fn is_relay_target(&self, to: Option<H160>) -> bool {
		if let Some(to) = to {
			return self.target_contracts.iter().any(|c| *c == to)
		}
		false
	}

	/// Verifies if the stored waiting block has waited enough.
	fn is_block_confirmed(&self, latest_block: U64) -> bool {
		latest_block.saturating_sub(self.waiting_block) > self.client.block_confirmations
	}

	/// Verifies if the connected provider is in block sync mode.
	pub async fn wait_provider_sync(&self) {
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
