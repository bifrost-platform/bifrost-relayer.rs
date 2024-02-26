use std::{cmp::max, fmt::Debug, sync::Arc};

use br_primitives::{
	constants::{
		config::{BOOTSTRAP_BLOCK_OFFSET, NATIVE_BLOCK_TIME},
		errors::{INSUFFICIENT_FUNDS, INVALID_CHAIN_ID, PROVIDER_INTERNAL_ERROR},
		tx::{DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS},
	},
	contracts::authority::RoundMetaData,
	eth::{AggregatorContracts, ChainID, ProtocolContracts, ProviderMetadata},
	sub_display_format,
};
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

use self::{
	traits::{Eip1559GasMiddleware, LegacyGasMiddleware},
	wallet::WalletManager,
};

pub mod events;
pub mod handlers;
pub mod traits;
pub mod tx;
pub mod wallet;

const SUB_LOG_TARGET: &str = "eth-client";

/// The core client for EVM-based chain interactions.
pub struct EthClient<T> {
	/// The wallet manager for the connected relayer.
	pub wallet: WalletManager,
	/// The metadata of the provider.
	pub metadata: ProviderMetadata,
	/// the protocol contract instances of the provider.
	pub protocol_contracts: ProtocolContracts<T>,
	/// the aggregator contract instances of the provider.
	pub aggregator_contracts: AggregatorContracts<T>,
	/// The ethers.rs wrapper for the connected chain.
	provider: Arc<Provider<T>>,
	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged such as
	/// gas estimation failures.
	debug_mode: bool,
}

impl<T: JsonRpcClient> EthClient<T> {
	/// Instantiates a new `EthClient` instance for the given chain.
	pub fn new(
		wallet: WalletManager,
		provider: Arc<Provider<T>>,
		metadata: ProviderMetadata,
		protocol_contracts: ProtocolContracts<T>,
		aggregator_contracts: AggregatorContracts<T>,
		debug_mode: bool,
	) -> Self {
		Self { wallet, provider, metadata, protocol_contracts, aggregator_contracts, debug_mode }
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
	/// Approximately Bifrost: 3s, Polygon: 2s, BSC: 3s, Ethereum: 12s
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
			let timestamp_diff =
				current_block.timestamp.checked_sub(prev_block.timestamp).unwrap().as_u64() as f64;
			let block_time = timestamp_diff / BOOTSTRAP_BLOCK_OFFSET as f64;

			let blocks = round_offset.checked_mul(round_info.round_length.as_u32()).unwrap();
			let blocks_to_native_chain_time = blocks.checked_mul(NATIVE_BLOCK_TIME).unwrap();
			let bootstrap_offset_height = blocks_to_native_chain_time as f64 / block_time;
			(bootstrap_offset_height.ceil() as u32).into()
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

#[async_trait::async_trait]
impl<T: JsonRpcClient> LegacyGasMiddleware for EthClient<T> {
	async fn get_gas_price(&self) -> U256 {
		match self.provider.get_gas_price().await {
			Ok(gas_price) => {
				br_metrics::increase_rpc_calls(&self.get_chain_name());
				gas_price
			},
			Err(error) => {
				self.handle_failed_get_gas_price(DEFAULT_CALL_RETRIES, error.to_string()).await
			},
		}
	}

	async fn get_gas_price_for_retry(
		&self,
		previous_gas_price: U256,
		gas_price_coefficient: f64,
		min_gas_price: U256,
	) -> U256 {
		let previous_gas_price = previous_gas_price.as_u64() as f64;

		let current_gas_price = self.get_gas_price().await;
		let escalated_gas_price =
			U256::from((previous_gas_price * gas_price_coefficient).ceil() as u64);

		max(max(current_gas_price, escalated_gas_price), min_gas_price)
	}

	async fn get_gas_price_for_escalation(
		&self,
		gas_price: U256,
		gas_price_coefficient: f64,
		min_gas_price: U256,
	) -> U256 {
		max(
			U256::from((gas_price.as_u64() as f64 * gas_price_coefficient).ceil() as u64),
			min_gas_price,
		)
	}

	async fn handle_failed_get_gas_price(&self, retries_remaining: u8, error: String) -> U256 {
		let mut retries = retries_remaining;
		let mut last_error = error;

		while retries > 0 {
			br_metrics::increase_rpc_calls(&self.get_chain_name());

			if self.debug_mode {
				log::warn!(
					target: &self.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during get gas price, Retries left: {:?}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					retries - 1,
					last_error
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during get gas price, Retries left: {:?}, Error: {}",
						&self.get_chain_name(),
						SUB_LOG_TARGET,
						self.address(),
						retries - 1,
						last_error
					)
					.as_str(),
					sentry::Level::Warning,
				);
			}

			match self.provider.get_gas_price().await {
				Ok(gas_price) => return gas_price,
				Err(error) => {
					sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					retries -= 1;
					last_error = error.to_string();
				},
			}
		}

		panic!(
			"[{}]-[{}]-[{}] {} [method: get_gas_price]: {}",
			&self.get_chain_name(),
			SUB_LOG_TARGET,
			self.address(),
			PROVIDER_INTERNAL_ERROR,
			last_error
		);
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Eip1559GasMiddleware for EthClient<T> {
	async fn get_estimated_eip1559_fees(&self) -> (U256, U256) {
		match self.provider.estimate_eip1559_fees(None).await {
			Ok(fees) => {
				br_metrics::increase_rpc_calls(&self.get_chain_name());
				fees
			},
			Err(error) => {
				self.handle_failed_get_estimated_eip1559_fees(
					DEFAULT_CALL_RETRIES,
					error.to_string(),
				)
				.await
			},
		}
	}

	async fn handle_failed_get_estimated_eip1559_fees(
		&self,
		retries_remaining: u8,
		error: String,
	) -> (U256, U256) {
		let mut retries = retries_remaining;
		let mut last_error = error;

		while retries > 0 {
			br_metrics::increase_rpc_calls(&self.get_chain_name());

			if self.debug_mode {
				log::warn!(
					target: &self.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during get estimated eip1559 fees, Retries left: {:?}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					retries - 1,
					last_error
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during get estimated eip1559 fees, Retries left: {:?}, Error: {}",
						&self.get_chain_name(),
						SUB_LOG_TARGET,
						self.address(),
						retries - 1,
						last_error
					)
						.as_str(),
					sentry::Level::Warning,
				);
			}

			match self.provider.estimate_eip1559_fees(None).await {
				Ok(fees) => return fees,
				Err(error) => {
					sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					retries -= 1;
					last_error = error.to_string();
				},
			}
		}

		panic!(
			"[{}]-[{}]-[{}] {} [method: get_estimated_eip1559_fees]: {}",
			&self.get_chain_name(),
			SUB_LOG_TARGET,
			self.address(),
			PROVIDER_INTERNAL_ERROR,
			last_error
		);
	}
}
