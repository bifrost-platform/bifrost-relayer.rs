mod events;
pub use events::*;

use cccp_primitives::eth::EthClientConfiguration;

use std::collections::VecDeque;
use web3::{
	ethabi::Address,
	types::{
		Block, BlockId, BlockNumber, Transaction, TransactionId, TransactionReceipt, H256, U256,
		U64,
	},
	Transport, Web3,
};

/// The core client for EVM-based chain interactions.
pub struct EthClient<T: Transport> {
	/// The web3 wrapper for the connected chain.
	web3: Web3<T>,
	/// The queue storing mined block numbers. The stored block will be popped when the queue size
	/// exceeds the maximum size.
	block_queue: VecDeque<U64>,
	/// The queue storing CCCP-related transaction events.
	_event_queue: Vec<EventData>,
	/// The specific configuration details for the connected chain.
	config: EthClientConfiguration,
}

impl<T: Transport> EthClient<T> {
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(url: &str, config: EthClientConfiguration) -> EthClient<web3::transports::Http> {
		match web3::transports::Http::new(url) {
			Ok(transport) => {
				let web3 = Web3::new(transport);
				EthClient { web3, block_queue: VecDeque::new(), _event_queue: vec![], config }
			},
			Err(error) => panic!("Error on initiating EthClient: {:?}", error),
		}
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

	/// Retrieves the balance of the given address.
	pub async fn get_balance(
		&self,
		address: Address,
		block: Option<BlockNumber>,
	) -> web3::Result<U256> {
		Ok(self.web3.eth().balance(address, block).await?)
	}

	/// Retrieves the latest mined block number of the connected chain.
	pub async fn get_latest_block_number(&self) -> web3::Result<U64> {
		Ok(self.web3.eth().block_number().await?)
	}

	/// Retrieves the block information of the given block hash.
	pub async fn get_block(&self, id: BlockId) -> web3::Result<Option<Block<H256>>> {
		Ok(self.web3.eth().block(id).await?)
	}

	/// Retrieves the transaction information of the given transaction hash.
	pub async fn get_transaction(&self, id: TransactionId) -> web3::Result<Option<Transaction>> {
		Ok(self.web3.eth().transaction(id).await?)
	}

	/// Retrieves the transaction receipt of the given transaction hash.
	pub async fn get_transaction_receipt(
		&self,
		hash: H256,
	) -> web3::Result<Option<TransactionReceipt>> {
		Ok(self.web3.eth().transaction_receipt(hash).await?)
	}
}
