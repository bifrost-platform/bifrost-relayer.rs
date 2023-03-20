mod events;
use std::sync::Arc;

pub use events::*;

mod tx;
pub use tx::*;

use ethers::{
	abi::RawLog,
	prelude::abigen,
	providers::{JsonRpcClient, Middleware, Provider},
	types::{Block, BlockId, TransactionReceipt, H256, U64},
};

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

#[derive(Clone, Debug)]
/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The ethers.rs wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The specific configuration details for the connected chain.
	config: EthClientConfiguration,
	pub socket: SocketExternal<Provider<T>>,
}

impl<T: JsonRpcClient> EthClient<T>
where
	Self: Send + Sync,
{
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(provider: Arc<Provider<T>>, config: EthClientConfiguration) -> Self {
		Self {
			provider: provider.clone(),
			config: config.clone(),
			socket: SocketExternal::new(config.socket_address, provider),
		}
	}

	/// Retrieves the latest mined block number of the connected chain.
	pub async fn get_latest_block_number(&self) -> EthResult<U64> {
		self.provider.get_block_number().await
	}

	/// Retrieves the block information of the given block hash.
	pub async fn get_block(&self, id: BlockId) -> EthResult<Option<Block<H256>>> {
		self.provider.get_block(id).await
	}

	/// Retrieves the transaction receipt of the given transaction hash.
	pub async fn get_transaction_receipt(
		&self,
		hash: H256,
	) -> EthResult<Option<TransactionReceipt>> {
		self.provider.get_transaction_receipt(hash).await
	}
}
