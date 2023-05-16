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
	abi::Detokenize,
	prelude::ContractCall,
	providers::{JsonRpcClient, Middleware, Provider},
	types::{
		Address, Block, BlockId, Filter, Log, SyncingStatus, Transaction, TransactionReceipt,
		TxpoolContent, H160, H256, U64,
	},
};
use serde::{de::DeserializeOwned, Serialize};
use std::{fmt::Debug, str::FromStr, sync::Arc};
use tokio::time::{sleep, Duration};

pub use cccp_primitives::contracts::*;
use cccp_primitives::{
	authority::AuthorityContract,
	eth::{BridgeDirection, ChainID},
	socket::SocketContract,
	vault::VaultContract,
	INVALID_CONTRACT_ADDRESS,
};

const SUB_LOG_TARGET: &str = "eth-client";

/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The wallet manager for the connected relayer.
	pub wallet: WalletManager,
	/// The ethers.rs wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The name of chain which this client interact with.
	name: String,
	/// Id of chain which this client interact with.
	id: ChainID,
	/// The number of confirmations required for a block to be processed.
	pub block_confirmations: U64,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// Bridge direction when bridge event points this chain as destination.
	pub if_destination_chain: BridgeDirection,
	/// The flag whether the chain is BIFROST(native) or an external chain.
	pub is_native: bool,
	/// SocketContract
	pub socket: SocketContract<Provider<T>>,
	/// VaultContract
	pub vault: VaultContract<Provider<T>>,
	/// AuthorityContract
	pub authority: AuthorityContract<Provider<T>>,
}

impl<T: JsonRpcClient> EthClient<T> {
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(
		wallet: WalletManager,
		provider: Arc<Provider<T>>,
		name: String,
		id: ChainID,
		block_confirmations: U64,
		call_interval: u64,
		is_native: bool,
		socket_address: String,
		vault_address: String,
		authority_address: String,
	) -> Self {
		Self {
			wallet,
			socket: SocketContract::new(
				H160::from_str(&socket_address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			),
			vault: VaultContract::new(
				H160::from_str(&vault_address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			),
			authority: AuthorityContract::new(
				H160::from_str(&authority_address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			),
			provider,
			name,
			id,
			block_confirmations,
			call_interval,
			if_destination_chain: match is_native {
				true => BridgeDirection::Inbound,
				false => BridgeDirection::Outbound,
			},
			is_native,
		}
	}

	/// Returns the relayer address.
	pub fn address(&self) -> Address {
		self.wallet.address()
	}

	/// Returns name which chain this client interacts with.
	pub fn get_chain_name(&self) -> String {
		self.name.clone()
	}

	/// Returns id which chain this client interacts with.
	pub fn get_chain_id(&self) -> ChainID {
		self.id
	}

	/// Returns `Arc<Provider>`.
	pub fn get_provider(&self) -> Arc<Provider<T>> {
		self.provider.clone()
	}

	/// Make an RPC request to the chain provider via the internal connection, and return the
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
			"[{}]-[{}] An internal error thrown when making a call to the provider. Please check your provider's status [method: {}]: {}",
			&self.get_chain_name(),
			SUB_LOG_TARGET,
			method,
			error_msg
		);
	}

	/// Make an contract call to the chain provider via the internal connection, and return the
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
			"[{}]-[{}] An internal error thrown when making a call to the provider. Please check your provider's status [method: {}]: {}",
			&self.get_chain_name(),
			SUB_LOG_TARGET,
			method,
			error_msg
		);
	}

	/// Retrieves the latest mined block number of the connected chain.
	pub async fn get_latest_block_number(&self) -> U64 {
		self.rpc_call("eth_blockNumber", ()).await
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
	pub async fn get_logs(&self, filter: Filter) -> Vec<Log> {
		self.rpc_call("eth_getLogs", vec![&filter]).await
	}

	/// Returns an object with data about the sync status or false.
	pub async fn is_syncing(&self) -> SyncingStatus {
		self.rpc_call("eth_syncing", ()).await
	}
}
