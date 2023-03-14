mod events;
pub use events::*;

use ethers::{
	providers::{JsonRpcClient, Middleware, Provider},
	types::{Block, BlockId, TransactionReceipt, H256, U64},
};
use std::{collections::VecDeque, sync::Arc};

use cccp_primitives::eth::{EthClientConfiguration, EthResult};

/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The web3 wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The queue storing mined block numbers. The stored block will be popped when the queue size
	/// exceeds the maximum size.
	block_queue: VecDeque<U64>,
	/// The queue storing CCCP-related transaction events.
	_event_queue: Vec<SocketMessage>,
	/// The specific configuration details for the connected chain.
	config: EthClientConfiguration,
}

impl<T: JsonRpcClient> EthClient<T> {
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(provider: Arc<Provider<T>>, config: EthClientConfiguration) -> Self {
		Self { provider, block_queue: VecDeque::new(), _event_queue: vec![], config }
	}

	/// Push a new block to the queue. Whenever the queue is full, the first element will be popped.
	pub fn try_push_block(&mut self, block_number: U64) -> Option<U64> {
		if self.block_queue.contains(&block_number) {
			return None
		}

		self.block_queue.push_back(block_number);
		if self.is_block_queue_full() {
			return self.block_queue.pop_front()
		}
		None
	}

	/// Verifies whether or not the block queue is full.
	fn is_block_queue_full(&self) -> bool {
		if self.config.block_queue_size < self.block_queue.len() as u64 {
			return true
		}
		false
	}

	/// Retrieves the latest mined block number of the connected chain.
	pub async fn get_latest_block_number(&self) -> EthResult<U64> {
		Ok(self.provider.get_block_number().await?)
	}

	/// Retrieves the block information of the given block hash.
	pub async fn get_block(&self, id: BlockId) -> EthResult<Option<Block<H256>>> {
		Ok(self.provider.get_block(id).await?)
	}

	/// Retrieves the transaction receipt of the given transaction hash.
	pub async fn get_transaction_receipt(
		&self,
		hash: H256,
	) -> EthResult<Option<TransactionReceipt>> {
		Ok(self.provider.get_transaction_receipt(hash).await?)
	}
}
