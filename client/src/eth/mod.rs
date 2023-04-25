mod events;
pub use events::*;

mod handlers;
pub use handlers::*;

mod tx;
pub use tx::*;

mod blocks;
pub use blocks::*;

mod wallet;
pub use wallet::*;

use ethers::{
	providers::{JsonRpcClient, Middleware, Provider},
	types::{Address, Block, BlockId, Transaction, TransactionReceipt, TxpoolContent, H256, U64},
};
use std::sync::Arc;

pub use cccp_primitives::contracts::*;
use cccp_primitives::eth::{EthClientConfiguration, EthResult};

/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The wallet manager for the connected relayer.
	pub wallet: WalletManager,
	/// The ethers.rs wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The specific configuration details for the connected chain.
	config: EthClientConfiguration,
	/// The flag whether the chain is BIFROST(native) or an external chain.
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
	pub fn address(&self) -> Address {
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

	/// Retrieves the transaction of the given transaction hash.
	pub async fn get_transaction(&self, hash: H256) -> EthResult<Option<Transaction>> {
		self.provider.get_transaction(hash).await
	}

	/// Retrieves the transaction receipt of the given transaction hash.
	pub async fn get_transaction_receipt(
		&self,
		hash: H256,
	) -> EthResult<Option<TransactionReceipt>> {
		self.provider.get_transaction_receipt(hash).await
	}

	/// Returns the details of all transactions currently pending for inclusion in the next
	/// block(s).
	pub async fn get_txpool_content(&self) -> EthResult<TxpoolContent> {
		self.provider.txpool_content().await
	}
}
