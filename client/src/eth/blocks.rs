use ethers::{
	providers::JsonRpcClient,
	types::{Block, TransactionReceipt, H160, H256, U64},
};
use std::sync::Arc;
use tokio::{
	sync::broadcast::{self, Receiver, Sender},
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

/// The essential task that listens and handle new blocks.
pub struct BlockManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The channel sending block messages.
	pub sender: Sender<BlockMessage>,
	/// The target contracts this chain is watching.
	pub target_contracts: Vec<H160>,
}

impl<T: JsonRpcClient> BlockManager<T> {
	/// Instantiates a new `BlockManager` instance.
	pub fn new(client: Arc<EthClient<T>>, target_contracts: Vec<H160>) -> Self {
		let (sender, _receiver) = broadcast::channel(512); // TODO: size?
		Self { client, sender, target_contracts }
	}

	/// Starts the block manager. Reads every new mined block of the connected chain and starts to
	/// publish to the block channel.
	pub async fn run(&self) {
		// TODO: follow-up to the highest block
		println!("target contracts -> {:?}", self.target_contracts);
		loop {
			// TODO: handle block reorgs
			let latest_block = self.client.get_latest_block_number().await.unwrap();
			self.process_confirmed_block(latest_block).await;

			println!(
				"[{:?}]-[block-manager] processed block: {:?}",
				self.client.get_chain_name(),
				latest_block
			);
			sleep(Duration::from_millis(self.client.config.call_interval)).await;
		}
	}

	/// Process the confirmed block and verifies if any action occurred from the target contracts.
	async fn process_confirmed_block(&self, block: U64) {
		if let Some(block) = self.client.get_block(block.into()).await.unwrap() {
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
				self.sender.send(BlockMessage::new(block, target_receipts)).unwrap();
			}
		}
	}

	/// Verifies if the transaction was occurred from the target contracts.
	fn is_in_target_contracts(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			return self.target_contracts.iter().any(|c| {
				ethers::utils::to_checksum(&c, None) == ethers::utils::to_checksum(&to, None)
			})
		}
		false
	}
}
