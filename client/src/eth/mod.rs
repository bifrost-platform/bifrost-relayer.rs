mod events;
pub use events::*;

mod handlers;
pub use handlers::*;

pub use cccp_primitives::contracts::*;

mod tx;
pub use tx::*;

mod blocks;
pub use blocks::*;

mod wallet;
pub use wallet::*;

use ethers::{
	providers::{JsonRpcClient, Middleware, Provider},
	types::{Block, BlockId, TransactionReceipt, H160, H256, U64},
};
use std::sync::Arc;

use cccp_primitives::eth::{EthClientConfiguration, EthResult};

pub type TxResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug)]
/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The wallet manager for the connected relayer.
	pub wallet: WalletManager,
	/// The ethers.rs wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The specific configuration details for the connected chain.
	config: EthClientConfiguration,
	/// Is bifrost network client?
	pub is_native: bool,
}

impl<T: JsonRpcClient> EthClient<T> {
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(
		wallet: WalletManager,
		provider: Arc<Provider<T>>,
		config: EthClientConfiguration,
		is_native: bool,
	) -> Self {
		Self { wallet, provider, config, is_native }
	}

	/// Returns the relayer address.
	pub fn address(&self) -> H160 {
		self.wallet.address()
	}

	/// Returns name which chain this client interacts with.
	pub fn get_chain_name(&self) -> String {
		self.config.name.clone()
	}

	/// Returns id which chain this client interacts with.
	pub fn get_chain_id(&self) -> u32 {
		self.config.id
	}

	/// Returns `Arc<Provider>`.
	pub fn get_provider(&self) -> Arc<Provider<T>> {
		self.provider.clone()
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
