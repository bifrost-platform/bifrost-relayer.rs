mod events;
pub use events::*;

use ethers::{
	abi::RawLog,
	prelude::abigen,
	providers::{JsonRpcClient, Middleware, Provider},
	types::{Block, BlockId, TransactionReceipt, H256, U64},
};
use std::{collections::VecDeque, sync::Arc};

use cccp_primitives::eth::{EthClientConfiguration, EthResult};

abigen!(
	SocketExternal,
	"../abi/abi.socket.external.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

#[derive(
	Clone,
	ethers::contract::EthEvent,
	ethers::contract::EthDisplay,
	Default,
	Debug,
	PartialEq,
	Eq,
	Hash,
)]
#[ethevent(
	name = "Socket",
	abi = "Socket(((bytes4,uint64,uint128),uint8,(bytes4,bytes16),(bytes32,bytes32,address,address,uint256,bytes)))"
)]
pub struct Socket {
	pub msg: SocketMessage,
}

#[derive(Clone, ethers::contract::EthAbiType, Debug, PartialEq, Eq, Hash)]
pub enum SocketEvents {
	Socket(Socket),
}

impl ethers::contract::EthLogDecode for SocketEvents {
	fn decode_log(log: &RawLog) -> Result<Self, ethers::abi::Error>
	where
		Self: Sized,
	{
		if let Ok(decoded) = Socket::decode_log(log) {
			return Ok(SocketEvents::Socket(decoded))
		}
		Err(ethers::abi::Error::InvalidData)
	}
}

/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The ethers.rs wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The queue storing mined block numbers. The stored block will be popped when the queue size
	/// exceeds the maximum size.
	block_queue: VecDeque<U64>,
	/// The queue storing CCCP socket transaction events.
	event_queue: Vec<SocketMessage>,
	/// The specific configuration details for the connected chain.
	config: EthClientConfiguration,
}

impl<T: JsonRpcClient> EthClient<T> {
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(provider: Arc<Provider<T>>, config: EthClientConfiguration) -> Self {
		Self { provider, block_queue: VecDeque::new(), event_queue: vec![], config }
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

	/// Push a new socket message to the queue.
	pub fn push_event(&mut self, event: SocketMessage) {
		self.event_queue.push(event);
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
