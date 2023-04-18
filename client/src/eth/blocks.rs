use cccp_primitives::{
	authority_bifrost::{AuthorityBifrost, RoundMetaData},
	cli::BootstrapConfig,
	eth::BootstrapState,
	sub_display_format,
};
use ethers::{
	providers::{JsonRpcClient, Middleware, Provider},
	types::{Block, TransactionReceipt, H160, H256, U64},
};
use std::{str::FromStr, sync::Arc};
use tokio::{
	sync::{
		broadcast::{self, Receiver, Sender},
		Mutex,
	},
	time::{sleep, Duration},
};
use tokio_stream::StreamExt;

use super::EthClient;

#[derive(Clone, Debug)]
/// The message format passed through the block channel.
pub struct BlockMessage {
	/// The information of the processed block.
	pub raw_block: Block<H256>,
	/// The transaction receipts from the target contracts.
	pub target_receipts: Vec<TransactionReceipt>,
}

impl BlockMessage {
	pub fn new(raw_block: Block<H256>, target_receipts: Vec<TransactionReceipt>) -> Self {
		Self { raw_block, target_receipts }
	}
}

/// The message receiver connected to the block channel.
pub struct BlockReceiver {
	/// The chain ID of the block channel.
	pub id: u32,
	/// The message receiver.
	pub receiver: Receiver<BlockMessage>,
}

impl BlockReceiver {
	pub fn new(id: u32, receiver: Receiver<BlockMessage>) -> Self {
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
	/// The pending block waiting for some confirmations.
	pub pending_block: U64,
	/// Bootstrap config
	pub bootstrap_config: BootstrapConfig,
	/// State of bootstrapping
	pub is_bootstrapping_completed: Arc<Mutex<BootstrapState>>,
	/// The target Authority contract instance.
	pub authority: AuthorityBifrost<Provider<T>>,
	/// Bootstrapping required flag (self)
	bootstrap_required: bool,
}

impl<T: JsonRpcClient> BlockManager<T> {
	/// Instantiates a new `BlockManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		target_contracts: Vec<H160>,
		bootstrap_config: BootstrapConfig,
		is_bootstrapping_completed: Arc<Mutex<BootstrapState>>,
		authority_address: String,
		native_client: Arc<EthClient<T>>,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512); // TODO: size?

		let authority = AuthorityBifrost::new(
			H160::from_str(&authority_address).expect("Failed to parse the authority address"),
			native_client.get_provider(),
		);

		let bootstrap_required = bootstrap_config.is_enabled;

		Self {
			client,
			sender,
			target_contracts,
			pending_block: U64::default(),
			bootstrap_config,
			is_bootstrapping_completed,
			authority,
			bootstrap_required,
		}
	}

	/// Initialize block manager.
	async fn initialize(&mut self) {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 📃 Target contracts: {:?}",
			sub_display_format(SUB_LOG_TARGET),
			self.target_contracts
		);

		if !self.bootstrap_config.is_enabled {
			self.pending_block = self.client.get_latest_block_number().await.unwrap();

			if let Some(block) = self.client.get_block(self.pending_block.into()).await.unwrap() {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 💤 Idle, best: #{:?} ({})",
					sub_display_format(SUB_LOG_TARGET),
					block.number.unwrap(),
					block.hash.unwrap(),
				);
			}
		}
	}

	/// Starts the block manager. Reads every new mined block of the connected chain and starts to
	/// publish to the block channel.
	pub async fn run(&mut self) {
		self.initialize().await;

		loop {
			if self.bootstrap_required {
				self.set_pending_block().await;
			}

			let latest_block = self.client.get_latest_block_number().await.unwrap();
			if self.is_block_confirmed(latest_block) {
				self.process_pending_block().await;
				self.increment_pending_block();
			}

			sleep(Duration::from_millis(self.client.config.call_interval)).await;
		}
	}

	/// Process the pending block and verifies if any action occurred from the target contracts.
	async fn process_pending_block(&self) {
		if let Some(block) = self.client.get_block(self.pending_block.into()).await.unwrap() {
			let mut target_receipts = vec![];
			let mut stream = tokio_stream::iter(block.clone().transactions);

			while let Some(tx) = stream.next().await {
				if let Some(receipt) = self.client.get_transaction_receipt(tx).await.unwrap() {
					if self.is_in_target_contracts(&receipt) {
						target_receipts.push(receipt);
					}
				}
			}
			if !target_receipts.is_empty() {
				self.sender.send(BlockMessage::new(block.clone(), target_receipts)).unwrap();
			}

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ✨ Imported #{:?}, latest: #{:?}",
				sub_display_format(SUB_LOG_TARGET),
				block.number.unwrap(),
				self.client.get_latest_block_number().await.unwrap(),
			);
		}
	}

	/// Increment the pending block.
	fn increment_pending_block(&mut self) {
		self.pending_block = self.pending_block.saturating_add(U64::from(1u64));
	}

	/// Verifies if the transaction was occurred from the target contracts.
	fn is_in_target_contracts(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			return self.target_contracts.iter().any(|c| {
				ethers::utils::to_checksum(c, None) == ethers::utils::to_checksum(&to, None)
			})
		}
		false
	}

	/// Verifies if the stored pending block waited for confirmations.
	fn is_block_confirmed(&self, latest_block: U64) -> bool {
		latest_block.saturating_sub(self.pending_block) > self.client.config.block_confirmations
	}

	/// Get factor between the block time of native-chain and block time of this chain
	/// Approximately bfc-testnet: 3s, matic-mumbai: 2s, bsc-testnet: 3s, eth-goerli: 15s
	async fn get_bootstrap_offset_height_based_on_block_time(&self, round_offset: u32) -> U64 {
		let block_offset = 100u32;
		let native_block_time = 3u32;
		let round_info: RoundMetaData = self.authority.round_info().call().await.unwrap();

		let block_number = self.client.provider.get_block_number().await.unwrap();

		let current_block = self.client.get_block((block_number).into()).await.unwrap().unwrap();
		let prev_block = self
			.client
			.get_block((block_number - block_offset).into())
			.await
			.unwrap()
			.unwrap();

		let diff = current_block
			.timestamp
			.checked_sub(prev_block.timestamp)
			.unwrap()
			.checked_div(block_offset.into())
			.unwrap();

		round_offset
			.checked_mul(round_info.round_length.as_u32())
			.unwrap()
			.checked_mul(native_block_time)
			.unwrap()
			.checked_div(diff.as_u32())
			.unwrap()
			.into()
	}

	async fn set_pending_block(&mut self) {
		// initialize pending block to the bootstrapping block
		if *self.is_bootstrapping_completed.lock().await != BootstrapState::NormalStart {
			// Before or After completion of Bootstrapping
			let bootstrap_offset_height = self
				.get_bootstrap_offset_height_based_on_block_time(self.bootstrap_config.round_offset)
				.await;

			self.pending_block = self
				.client
				.get_latest_block_number()
				.await
				.unwrap()
				.saturating_sub(bootstrap_offset_height);

			if *self.is_bootstrapping_completed.lock().await == BootstrapState::BootstrapSocket {
				*self.is_bootstrapping_completed.lock().await = BootstrapState::NormalStart;
			}
		} else {
			self.bootstrap_required = false;
		}
	}
}
