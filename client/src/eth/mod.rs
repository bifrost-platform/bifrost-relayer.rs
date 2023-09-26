use std::{fmt::Debug, sync::Arc};

use ethers::{
	abi::Detokenize,
	prelude::ContractCall,
	providers::{JsonRpcClient, Middleware, Provider},
	types::{
		Address, Block, BlockId, Filter, Log, SyncingStatus, Transaction, TransactionReceipt,
		TxpoolContent, H256, U256, U64,
	},
	utils::{format_units, WEI_IN_ETHER},
};
use serde::{de::DeserializeOwned, Serialize};
use tokio::time::{sleep, Duration};

pub use blocks::*;
pub use br_primitives::contracts::*;
use br_primitives::{
	authority::RoundMetaData,
	eth::{
		ChainID, ProviderContracts, ProviderMetadata, BOOTSTRAP_BLOCK_OFFSET, NATIVE_BLOCK_TIME,
	},
	INSUFFICIENT_FUNDS, INVALID_CHAIN_ID, PROVIDER_INTERNAL_ERROR,
};
pub use events::*;
pub use handlers::*;
pub use tx::*;
pub use wallet::*;

mod blocks;
mod events;
mod handlers;
mod tx;
mod wallet;

const SUB_LOG_TARGET: &str = "eth-client";

/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The wallet manager for the connected relayer.
	pub wallet: WalletManager,
	/// The ethers.rs wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The metadata of the provider.
	pub metadata: ProviderMetadata,
	/// the contract instances of the provider.
	pub contracts: ProviderContracts<T>,
}

impl<T: JsonRpcClient> EthClient<T> {
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(
		wallet: WalletManager,
		provider: Arc<Provider<T>>,
		metadata: ProviderMetadata,
		contracts: ProviderContracts<T>,
	) -> Self {
		Self { wallet, provider, metadata, contracts }
	}

	/// Returns the relayer address.
	pub fn address(&self) -> Address {
		self.wallet.address()
	}

	/// Returns name which chain this client interacts with.
	pub fn get_chain_name(&self) -> String {
		self.metadata.name.clone()
	}

	/// Returns id which chain this client interacts with.
	pub fn get_chain_id(&self) -> ChainID {
		self.metadata.id
	}

	/// Returns `Arc<Provider>`.
	pub fn get_provider(&self) -> Arc<Provider<T>> {
		self.provider.clone()
	}

	/// Make a JSON RPC request to the chain provider via the internal connection, and return the
	/// result. This method wraps the original JSON RPC call and retries whenever the request fails
	/// until it exceeds the maximum retries.
	async fn rpc_call<P, R>(&self, method: &str, params: P) -> R
	where
		P: Debug + Serialize + Send + Sync + Clone,
		R: Serialize + DeserializeOwned + Debug + Send,
	{
		let mut retries_remaining: u8 = DEFAULT_CALL_RETRIES;
		let mut error_msg = String::default();

		while retries_remaining > 0 {
			br_metrics::increase_rpc_calls(&self.get_chain_name());
			match self.provider.request(method, params.clone()).await {
				Ok(result) => return result,
				Err(error) => {
					// retry on error
					retries_remaining = retries_remaining.saturating_sub(1);
					error_msg = error.to_string();
				},
			}
			sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
		}
		panic!(
			"[{}]-[{}]-[{}] {} [method: {}]: {}",
			&self.get_chain_name(),
			SUB_LOG_TARGET,
			self.address(),
			PROVIDER_INTERNAL_ERROR,
			method,
			error_msg
		);
	}

	/// Make a contract call to the chain provider via the internal connection, and return the
	/// result. This method wraps the original contract call and retries whenever the request fails
	/// until it exceeds the maximum retries.
	pub async fn contract_call<M, D>(&self, raw_call: ContractCall<M, D>, method: &str) -> D
	where
		M: Middleware,
		D: Serialize + DeserializeOwned + Debug + Send + Detokenize,
	{
		let mut retries_remaining: u8 = DEFAULT_CALL_RETRIES;
		let mut error_msg = String::default();

		while retries_remaining > 0 {
			br_metrics::increase_rpc_calls(&self.get_chain_name());
			match raw_call.call().await {
				Ok(result) => return result,
				Err(error) => {
					// retry on error
					retries_remaining = retries_remaining.saturating_sub(1);
					error_msg = error.to_string();
				},
			}
			sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
		}
		panic!(
			"[{}]-[{}]-[{}] {} [method: {}]: {}",
			&self.get_chain_name(),
			SUB_LOG_TARGET,
			self.address(),
			PROVIDER_INTERNAL_ERROR,
			method,
			error_msg
		);
	}

	/// Verifies whether the configured chain ID and the provider's actual chain ID matches.
	pub async fn verify_chain_id(&self) {
		let chain_id: U256 = self.rpc_call("eth_chainId", ()).await;
		if self.get_chain_id() != chain_id.as_u32() {
			panic!(
				"[{}]-[{}]-[{}] {}",
				&self.get_chain_name(),
				SUB_LOG_TARGET,
				self.address(),
				INVALID_CHAIN_ID
			);
		}
	}

	/// Verifies whether the relayer has at least the minimum balance required.
	pub async fn verify_minimum_balance(&self) {
		if self.metadata.is_native {
			let balance = self.get_balance(self.address()).await;
			if balance < WEI_IN_ETHER {
				panic!(
					"[{}]-[{}]-[{}] {}",
					&self.get_chain_name(),
					SUB_LOG_TARGET,
					self.address(),
					INSUFFICIENT_FUNDS
				);
			}
		}
	}

	/// Retrieves the balance of the given address.
	pub async fn get_balance(&self, who: Address) -> U256 {
		self.rpc_call("eth_getBalance", (who, "latest")).await
	}

	/// Retrieves the latest mined block number of the connected chain.
	pub async fn get_latest_block_number(&self) -> U64 {
		self.rpc_call("eth_blockNumber", ()).await
	}

	/// Retrieves the block information of the given block hash.
	pub async fn get_block_with_txs(&self, id: BlockId) -> Option<Block<Transaction>> {
		self.rpc_call("eth_getBlockByNumber", (id, true)).await
	}

	/// Retrieves the block information of the given block hash.
	pub async fn get_block(&self, id: BlockId) -> Option<Block<H256>> {
		self.rpc_call("eth_getBlockByNumber", (id, false)).await
	}

	/// Retrieves the transaction of the given transaction hash.
	pub async fn get_transaction(&self, hash: H256) -> Option<Transaction> {
		self.rpc_call("eth_getTransactionByHash", vec![hash]).await
	}

	/// Retrieves the transaction receipt of the given transaction hash.
	pub async fn get_transaction_receipt(&self, hash: H256) -> Option<TransactionReceipt> {
		self.rpc_call("eth_getTransactionReceipt", vec![hash]).await
	}

	/// Returns the details of all transactions currently pending for inclusion in the next
	/// block(s).
	pub async fn get_txpool_content(&self) -> TxpoolContent {
		self.rpc_call("txpool_content", ()).await
	}

	/// Returns an array of all logs matching the given filter.
	pub async fn get_logs(&self, filter: &Filter) -> Vec<Log> {
		self.rpc_call("eth_getLogs", vec![filter]).await
	}

	/// Returns an object with data about the sync status or false.
	pub async fn is_syncing(&self) -> SyncingStatus {
		self.rpc_call("eth_syncing", ()).await
	}

	/// Get factor between the block time of native-chain and block time of this chain
	/// Approximately BIFROST: 3s, Polygon: 2s, BSC: 3s, Ethereum: 12s
	pub async fn get_bootstrap_offset_height_based_on_block_time(
		&self,
		round_offset: u32,
		round_info: RoundMetaData,
	) -> U64 {
		let block_number = self.get_latest_block_number().await;

		if let (Some(current_block), Some(prev_block)) = (
			self.get_block(block_number.into()).await,
			self.get_block((block_number - BOOTSTRAP_BLOCK_OFFSET).into()).await,
		) {
			let diff = current_block
				.timestamp
				.checked_sub(prev_block.timestamp)
				.unwrap()
				.checked_div(BOOTSTRAP_BLOCK_OFFSET.into())
				.unwrap();

			round_offset
				.checked_mul(round_info.round_length.as_u32())
				.unwrap()
				.checked_mul(NATIVE_BLOCK_TIME)
				.unwrap()
				.checked_div(diff.as_u32())
				.unwrap()
				.into()
		} else {
			panic!(
				"[{}]-[{}]-[{}] {} [method: bootstrap]",
				&self.get_chain_name(),
				SUB_LOG_TARGET,
				PROVIDER_INTERNAL_ERROR,
				self.address()
			);
		}
	}

	/// Send prometheus metric of the current balance.
	pub async fn sync_balance(&self) {
		br_metrics::set_native_balance(
			&self.get_chain_name(),
			format_units(self.get_balance(self.address()).await, "ether")
				.unwrap()
				.parse::<f64>()
				.unwrap(),
		);
	}
}
