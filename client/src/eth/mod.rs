use br_primitives::{
	constants::{
		config::{BOOTSTRAP_BLOCK_OFFSET, NATIVE_BLOCK_TIME},
		errors::{INVALID_CHAIN_ID, PROVIDER_INTERNAL_ERROR},
		tx::DEFAULT_TX_TIMEOUT_MS,
	},
	contracts::{
		authority::BfcStaking::round_meta_data,
		erc20::{Erc20Contract, Erc20Instance},
		oracle::{OracleContract, OracleInstance},
	},
	eth::{
		AggregatorContracts, ContractCache, GasCoefficient, ProtocolContracts, ProviderMetadata,
		Signers,
	},
	tx::TxRequestMetadata,
	utils::sub_display_format,
};

use alloy::{
	consensus::{BlockHeader, Transaction, TxType, Typed2718},
	eips::BlockNumberOrTag,
	network::{BlockResponse, Network, TransactionBuilder, TransactionResponse},
	primitives::{
		Address, ChainId, FixedBytes, U64, keccak256,
		utils::{Unit, format_units},
	},
	providers::{
		EthCall, PendingTransactionBuilder, Provider, RootProvider, SendableTx, WalletProvider,
		ext::TxPoolApi as _,
		fillers::{FillProvider, TxFiller},
	},
	signers::Signature,
	transports::TransportResult,
};
use eyre::{Result, eyre};
use rand::Rng as _;
use sc_service::SpawnTaskHandle;
use std::{
	cmp::max,
	collections::{BTreeMap, VecDeque},
	sync::Arc,
	time::Duration,
};
use tokio::{
	sync::{Mutex, RwLock},
	time::sleep,
};
use url::Url;

pub mod events;
pub mod handlers;
pub mod traits;

pub type ClientMap<F, P, N> = BTreeMap<ChainId, Arc<EthClient<F, P, N>>>;

const SUB_LOG_TARGET: &str = "eth-client";

#[derive(Clone)]
pub struct EthClient<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The inner provider.
	inner: Arc<FillProvider<F, P, N>>,
	/// The signers.
	signers: Signers,
	/// The default signer address.
	default_address: Arc<RwLock<Address>>,
	/// The provider metadata.
	pub metadata: ProviderMetadata,
	/// The protocol contracts.
	pub protocol_contracts: ProtocolContracts<F, P, N>,
	/// The aggregator contracts.
	pub aggregator_contracts: AggregatorContracts<F, P, N>,
	/// Contract cache for oracle and ERC20 instances.
	pub contract_cache: Arc<ContractCache<F, P, N>>,
	/// flushing not allowed to work concurrently.
	pub martial_law: Arc<Mutex<()>>,
}

impl<F, P, N: Network> EthClient<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Create a new EthClient
	pub fn new(
		inner: Arc<FillProvider<F, P, N>>,
		signers: Signers,
		default_address: Arc<RwLock<Address>>,
		metadata: ProviderMetadata,
		protocol_contracts: ProtocolContracts<F, P, N>,
		aggregator_contracts: AggregatorContracts<F, P, N>,
	) -> Self {
		Self {
			inner,
			signers,
			default_address,
			metadata,
			protocol_contracts,
			aggregator_contracts,
			contract_cache: Arc::new(ContractCache::new()),
			martial_law: Arc::new(Mutex::new(())),
		}
	}

	/// Verifies whether the configured chain id and the provider's chain id match.
	pub async fn verify_chain_id(&self) -> Result<()> {
		let chain_id = self.get_chain_id().await?;
		if chain_id != self.metadata.id { Err(eyre!(INVALID_CHAIN_ID)) } else { Ok(()) }
	}

	/// Get the default signer address.
	pub async fn address(&self) -> Address {
		*self.default_address.read().await
	}

	/// Get the signer addresses.
	pub fn signers(&self) -> Vec<Address> {
		self.signers.signers_address()
	}

	/// Set the default signer address.
	pub async fn set_address(&self, address: Address) {
		*self.default_address.write().await = address;
	}

	/// Update the default signer address.
	pub async fn update_default_address(&self, new_relayers: Option<&[Address]>) {
		let relayers = match new_relayers {
			Some(relayers) => relayers.to_vec(),
			None => {
				let relayer_manager = self.protocol_contracts.relayer_manager.as_ref().unwrap();
				relayer_manager.selected_relayers(true).call().await.unwrap()
			},
		};

		let matched = match self.signers().iter().find(|s| relayers.contains(s)) {
			Some(selected) => *selected,
			None => Address::default(),
		};

		let before_update = self.address().await;
		if before_update != matched {
			log::info!(
				target: &self.get_chain_name(),
				"-[{}] 👤 Relayer updated: {} -> {}",
				sub_display_format(SUB_LOG_TARGET),
				before_update,
				matched
			);
			self.set_address(matched).await;
		}
	}

	/// Get the chain name.
	pub fn get_chain_name(&self) -> String {
		self.metadata.name.clone()
	}

	/// Returns the URL of the provider.
	pub fn get_url(&self) -> Url {
		self.metadata.url.clone()
	}
	/// Sync the native token balance to the metrics.
	pub async fn sync_balance(&self) -> Result<()> {
		br_metrics::set_native_balance(
			&self.get_chain_name(),
			format_units(self.get_balance(self.address().await).await?, "ether")?.parse::<f64>()?,
		);
		Ok(())
	}

	/// Get the chain id.
	pub fn chain_id(&self) -> ChainId {
		self.metadata.id
	}

	/// Get the bitcoin chain id.
	pub fn get_bitcoin_chain_id(&self) -> Option<ChainId> {
		self.metadata.bitcoin_chain_id
	}

	/// Signs the given message.
	pub async fn sign_message(&self, message: &[u8]) -> Result<Signature> {
		let signer = self.signers.get_signer(&self.address().await).unwrap();
		Ok(signer.sign_hash(&keccak256(message)).await?)
	}

	/// Get the bootstrap offset height based on the block time.
	/// Approximately Bifrost: 3s, Polygon: 2s, BSC: 3s, Ethereum: 12s
	pub async fn get_bootstrap_offset_height_based_on_block_time(
		&self,
		round_offset: u64,
		round_info: round_meta_data,
	) -> Result<u64> {
		let block_number = self.get_block_number().await?;
		let prev_block_number = block_number.saturating_sub(BOOTSTRAP_BLOCK_OFFSET);
		let block_diff = block_number.checked_sub(prev_block_number).unwrap();

		let current_block = self.get_block(block_number.into()).hashes().await?;
		let prev_block = self.get_block(prev_block_number.into()).hashes().await?;
		match (current_block, prev_block) {
			(Some(current_block), Some(prev_block)) => {
				let current_timestamp = current_block.header().timestamp();
				let prev_timestamp = prev_block.header().timestamp();
				let timestamp_diff = current_timestamp.checked_sub(prev_timestamp).unwrap() as f64;
				let avg_block_time = timestamp_diff / block_diff as f64;

				let blocks = round_offset
					.checked_mul(round_info.round_length.saturating_to::<u64>())
					.unwrap();
				let blocks_to_native_chain_time = blocks.checked_mul(NATIVE_BLOCK_TIME).unwrap();
				let bootstrap_offset_height = blocks_to_native_chain_time as f64 / avg_block_time;
				Ok(bootstrap_offset_height.ceil() as u64)
			},
			_ => Err(eyre!(PROVIDER_INTERNAL_ERROR)),
		}
	}

	/// Verifies whether the current relayer was selected at the current round
	pub async fn is_selected_relayer(&self) -> Result<bool> {
		let relayer_manager = self.protocol_contracts.relayer_manager.as_ref().unwrap();
		Ok(relayer_manager.is_selected_relayer(self.address().await, false).call().await?)
	}

	/// Check the blaze activation state
	pub async fn blaze_activation(&self) -> Result<bool> {
		if let Some(blaze) = self.protocol_contracts.blaze.as_ref() {
			Ok(blaze.is_activated().call().await?)
		} else {
			Ok(false)
		}
	}

	/// Fetches historical logs for bootstrap operations with automatic chunking.
	///
	/// This helper method reduces boilerplate in bootstrap handlers by:
	/// 1. Calculating the bootstrap offset height based on round configuration
	/// 2. Splitting the block range into chunks to avoid RPC limits
	/// 3. Fetching and aggregating logs from all chunks
	///
	/// # Arguments
	/// * `bootstrap_config` - Optional bootstrap configuration with round offset
	/// * `addresses` - Contract addresses to filter logs
	/// * `event_signature` - Event signature hash to filter
	/// * `chunk_size` - Number of blocks per chunk (use BOOTSTRAP_BLOCK_CHUNK_SIZE)
	///
	/// # Returns
	/// A vector of logs matching the filter criteria
	pub async fn get_historical_logs(
		&self,
		round_offset: u64,
		addresses: Vec<Address>,
		event_signature: alloy::primitives::B256,
		chunk_size: u64,
	) -> Result<Vec<alloy::rpc::types::Log>> {
		use alloy::rpc::types::Filter;

		let round_info = self.protocol_contracts.authority.round_info().call().await?;
		let bootstrap_offset_height = self
			.get_bootstrap_offset_height_based_on_block_time(round_offset, round_info)
			.await?;

		let latest_block_number = self.get_block_number().await?;
		let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
		let to_block = latest_block_number;

		let mut logs = vec![];

		while from_block <= to_block {
			let chunk_to_block = std::cmp::min(from_block + chunk_size - 1, to_block);

			let filter = Filter::new()
				.address(addresses.clone())
				.event_signature(event_signature)
				.from_block(from_block)
				.to_block(chunk_to_block);

			let chunk_logs = self.get_logs(&filter).await?;
			logs.extend(chunk_logs);

			from_block = chunk_to_block + 1;
		}

		Ok(logs)
	}

	/// Flush stalled transactions from the txpool.
	pub async fn flush_stalled_transactions(&self) -> Result<()> {
		let _lock = self.martial_law.lock().await;

		log::info!(target: &self.get_chain_name(), "-[{}] Flushing stalled transactions", sub_display_format(SUB_LOG_TARGET));

		// if the chain is native or txpool is not enabled, do nothing
		if self.metadata.is_native || self.txpool_status().await.is_err() {
			return Ok(());
		}

		// possibility of txpool being flushed automatically. wait for 2 blocks.
		sleep(Duration::from_millis(self.metadata.call_interval * 2)).await;

		let address = self.address().await;
		let content = self.txpool_content().await?.remove_from(&address);
		let mut pending = content.pending;
		pending.extend(content.queued);

		if pending.is_empty() {
			return Ok(());
		}

		let mut transactions = pending
			.into_values()
			.map(|tx| {
				let mut request = N::TransactionRequest::default()
					.with_from(address)
					.with_to(tx.to().unwrap())
					.with_input(tx.input().clone())
					.with_nonce(tx.nonce());
				if tx.is_eip1559() {
					request.set_max_fee_per_gas(
						TransactionResponse::max_fee_per_gas(&tx).unwrap_or(0),
					);
					request
						.set_max_priority_fee_per_gas(tx.max_priority_fee_per_gas().unwrap_or(0));
				} else {
					request.set_gas_price(TransactionResponse::gas_price(&tx).unwrap_or(0));
				}
				request
			})
			.collect::<VecDeque<_>>();
		transactions.make_contiguous().sort_by_key(|a| a.nonce().unwrap());

		// if the nonce of the first transaction is not equal to the current nonce, update the nonce
		let mut count = self.get_transaction_count(address).await?;
		if transactions.front().unwrap().nonce().unwrap() != count {
			for tx in transactions.iter_mut() {
				tx.set_nonce(count);
				count += 1;
			}
		}

		while let Some(mut tx_request) = transactions.pop_front() {
			// RBF
			match TxType::try_from(tx_request.output_tx_type().into())? {
				TxType::Legacy => {
					let new_gas_price =
						((tx_request.gas_price().unwrap() as f64) * 1.1).ceil() as u128;
					let current_gas_price = self.get_gas_price().await?;

					tx_request.set_gas_price(max(new_gas_price, current_gas_price));
				},
				TxType::Eip1559 => {
					let current_gas_price = self.estimate_eip1559_fees().await?;

					let new_max_fee_per_gas =
						(tx_request.max_fee_per_gas().unwrap() as f64 * 1.1).ceil() as u128;
					let new_max_priority_fee_per_gas =
						(tx_request.max_priority_fee_per_gas().unwrap() as f64 * 1.1).ceil()
							as u128;

					tx_request.set_max_fee_per_gas(max(
						new_max_fee_per_gas,
						current_gas_price.max_fee_per_gas,
					));
					tx_request.set_max_priority_fee_per_gas(max(
						new_max_priority_fee_per_gas,
						current_gas_price.max_priority_fee_per_gas,
					));
				},
				_ => {
					eyre::bail!("Unsupported transaction type: {}", tx_request.output_tx_type());
				},
			}

			match self
				.send_transaction(tx_request.clone())
				.await?
				.with_timeout(Some(Duration::from_millis(DEFAULT_TX_TIMEOUT_MS)))
				.watch()
				.await
			{
				Ok(tx_hash) => {
					log::info!(
						target: &self.get_chain_name(),
						" 🔖 Transaction confirmed ({} tx:{}): Flush",
						self.get_chain_name(),
						tx_hash,
					);
				},
				Err(err) => {
					br_primitives::log_and_capture_simple!(
						error,
						" ❗️ Failed to send transaction ({} address:{}): Flush, Error: {}",
						self.get_chain_name(),
						address,
						err
					);
					transactions.push_front(tx_request);
				},
			}
		}

		log::info!(target: &self.get_chain_name(), "-[{}] Flushing stalled transactions completed", sub_display_format(SUB_LOG_TARGET));

		Ok(())
	}

	/// Fill the gas-related fields for the given transaction.
	/// Skips gas estimation if gas limit is already set (e.g., for hook execution).
	async fn fill_gas(&self, request: &mut N::TransactionRequest) -> Result<()> {
		if request.from().is_none() {
			request.set_from(self.address().await);
		}

		// Skip gas estimation if gas limit is already set (pre-filled by caller)
		if request.gas_limit().is_some() {
			log::debug!(
				target: &self.get_chain_name(),
				"-[{}] ⏭️  Skipping gas estimation - gas limit already set: {}",
				sub_display_format(SUB_LOG_TARGET),
				request.gas_limit().unwrap()
			);
		} else {
			let gas = self.estimate_gas(request.clone()).await?;
			let coefficient: f64 = GasCoefficient::from(self.metadata.is_native).into();
			let estimated_gas = gas as f64 * coefficient;
			request.set_gas_limit(estimated_gas.ceil() as u64);
		}

		if self.metadata.is_native {
			// gas price is fixed to 1000 Gwei on bifrost network
			request.set_max_fee_per_gas(1000 * Unit::GWEI.wei().to::<u128>());
			request.set_max_priority_fee_per_gas(0);
		} else {
			// to avoid duplicate(will revert) external networks transactions
			let duration = Duration::from_millis(rand::rng().random_range(0..=12000));
			sleep(duration).await;

			if !self.metadata.eip1559 {
				request.set_gas_price(self.get_gas_price().await?);
			}
		}

		Ok(())
	}

	/// Send a transaction synchronously.
	async fn sync_send_transaction(
		&self,
		mut request: N::TransactionRequest,
		requester: String,
		metadata: TxRequestMetadata,
	) -> Result<()> {
		if !self.metadata.is_relay_target {
			return Ok(());
		}

		self.fill_gas(&mut request).await?;
		let pending = match self.send_transaction(request).await {
			Ok(pending) => pending,
			Err(err) => {
				br_primitives::log_and_capture_simple!(
					error,
					" ❗️ Failed to send transaction ({} address:{}): {}, Error: {}",
					self.get_chain_name(),
					self.address().await,
					metadata,
					err
				);

				eyre::bail!(err)
			},
		};
		match pending
			.with_timeout(Some(Duration::from_millis(DEFAULT_TX_TIMEOUT_MS)))
			.watch()
			.await
		{
			Ok(tx_hash) => {
				log::info!(
					target: &requester,
					" 🔖 Transaction confirmed ({} tx:{}): {}",
					self.get_chain_name(),
					tx_hash,
					metadata
				);
				Ok(())
			},
			Err(_) => {
				self.flush_stalled_transactions().await?;
				Ok(())
			},
		}
	}

	/// Get or create an oracle contract instance from cache.
	pub async fn get_oracle(&self, address: Address) -> Arc<OracleInstance<F, P, N>> {
		// Check cache first
		{
			let cache = self.contract_cache.oracles.read().await;
			if let Some(oracle) = cache.get(&address) {
				return oracle.clone();
			}
		}

		// Create new instance and cache it
		let oracle = Arc::new(OracleContract::new(address, self.inner.clone()));
		self.contract_cache.oracles.write().await.insert(address, oracle.clone());
		oracle
	}

	/// Get or create an ERC20 contract instance from cache.
	pub async fn get_erc20(&self, address: Address) -> Arc<Erc20Instance<F, P, N>> {
		// Check cache first
		{
			let cache = self.contract_cache.erc20s.read().await;
			if let Some(erc20) = cache.get(&address) {
				return erc20.clone();
			}
		}

		// Create new instance and cache it
		let erc20 = Arc::new(Erc20Contract::new(address, self.inner.clone()));
		self.contract_cache.erc20s.write().await.insert(address, erc20.clone());
		erc20
	}

	/// Get asset address with caching.
	pub async fn get_asset_address_by_index(
		&self,
		asset_index_hash: FixedBytes<32>,
	) -> Result<Address> {
		// Check cache first
		{
			let cache = self.contract_cache.asset_addresses.read().await;
			if let Some(&address) = cache.get(&asset_index_hash) {
				return Ok(address);
			}
		}

		// Fetch from vault and cache
		let address = self
			.protocol_contracts
			.vault
			.assets_config(asset_index_hash)
			.call()
			.await?
			.target;
		self.contract_cache
			.asset_addresses
			.write()
			.await
			.insert(asset_index_hash, address);
		Ok(address)
	}

	/// Get asset oracle address with caching.
	pub async fn get_oracle_address_by_asset_index(
		&self,
		asset_index_hash: FixedBytes<32>,
	) -> Result<Address> {
		// Check cache first
		{
			let cache = self.contract_cache.asset_oracle_addresses.read().await;
			if let Some(&address) = cache.get(&asset_index_hash) {
				return Ok(address);
			}
		}

		// Fetch from relay queue and cache
		let relay_queue = self
			.protocol_contracts
			.relay_queue
			.as_ref()
			.ok_or(eyre::eyre!("RelayQueue contract not available"))?;

		let address = relay_queue.get_asset_oracle_by_hash(asset_index_hash).call().await?;
		self.contract_cache
			.asset_oracle_addresses
			.write()
			.await
			.insert(asset_index_hash, address);
		Ok(address)
	}

	/// Get native currency oracle address with caching.
	pub async fn get_oracle_address_by_chain(&self, chain_id: ChainId) -> Result<Address> {
		// Check cache first
		{
			let cache = self.contract_cache.native_oracle_addresses.read().await;
			if let Some(&address) = cache.get(&chain_id) {
				return Ok(address);
			}
		}

		// Fetch from relay queue and cache
		let relay_queue = self
			.protocol_contracts
			.relay_queue
			.as_ref()
			.ok_or(eyre::eyre!("RelayQueue contract not available"))?;

		let address = relay_queue.get_native_currency_oracle(chain_id as u32).call().await?;
		self.contract_cache
			.native_oracle_addresses
			.write()
			.await
			.insert(chain_id, address);
		Ok(address)
	}

	/// Get oracle decimals from cache or fetch and cache if not present.
	pub async fn get_oracle_decimals(&self, address: Address) -> Result<u8> {
		// Check cache first
		{
			let cache = self.contract_cache.oracle_decimals.read().await;
			if let Some(&decimals) = cache.get(&address) {
				return Ok(decimals);
			}
		}

		// Fetch and cache
		let oracle = self.get_oracle(address).await;
		let decimals = oracle.decimals().call().await?;
		self.contract_cache.oracle_decimals.write().await.insert(address, decimals);
		Ok(decimals)
	}

	/// Get ERC20 token decimals from cache or fetch and cache if not present.
	pub async fn get_erc20_decimals(&self, address: Address) -> Result<u8> {
		// Check cache first
		{
			let cache = self.contract_cache.erc20_decimals.read().await;
			if let Some(&decimals) = cache.get(&address) {
				return Ok(decimals);
			}
		}

		// Fetch and cache
		let erc20 = self.get_erc20(address).await;
		let decimals = erc20.decimals().call().await?;
		self.contract_cache.erc20_decimals.write().await.insert(address, decimals);
		Ok(decimals)
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> Provider<N> for EthClient<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn root(&self) -> &RootProvider<N> {
		self.inner.root()
	}

	fn estimate_gas(&self, tx: N::TransactionRequest) -> EthCall<N, U64, u64> {
		let call = EthCall::gas_estimate(self.inner.weak_client(), tx);

		if self.chain_id() == 56 || self.chain_id() == 97 {
			call.map_resp(|r| r.to::<u64>())
		} else {
			call.block(BlockNumberOrTag::Pending.into()).map_resp(|r| r.to::<u64>())
		}
	}

	async fn send_transaction_internal(
		&self,
		tx: SendableTx<N>,
	) -> TransportResult<PendingTransactionBuilder<N>> {
		self.inner.send_transaction_internal(tx).await
	}
}

/// Send a transaction asynchronously.
pub fn send_transaction<F, P, N: Network>(
	client: Arc<EthClient<F, P, N>>,
	mut request: N::TransactionRequest,
	requester: String,
	metadata: TxRequestMetadata,
	debug_mode: bool,
	handle: SpawnTaskHandle,
) where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	if !client.metadata.is_relay_target {
		return;
	}

	let this_handle = handle.clone();
	this_handle.spawn("send_transaction", None, async move {
		if let Err(err) = client.fill_gas(&mut request).await {
			if debug_mode {
				if err.to_string().contains("revert tx already executed") {
					return;
				}

				let msg = format!(
					" ❗️ Failed to estimate gas ({} address:{}): {}, Error: {}",
					client.get_chain_name(),
					client.address().await,
					metadata,
					err
				);
				log::error!(target: &requester, "{msg}");
				sentry::capture_message(&msg, sentry::Level::Error);
			}
			return;
		}

		match client.send_transaction(request.clone()).await {
			Ok(pending) => {
				log::info!(
					target: &requester,
					" 🔖 Send transaction ({} tx:{}): {}",
					client.get_chain_name(),
					pending.tx_hash(),
					metadata
				);

				match pending
					.with_timeout(Some(Duration::from_millis(DEFAULT_TX_TIMEOUT_MS)))
					.watch()
					.await
				{
					Ok(tx_hash) => {
						log::info!(
							target: &requester,
							" 🔖 Transaction confirmed ({} tx:{}): {}",
							client.get_chain_name(),
							tx_hash,
							metadata
						);
					},
					Err(err) => {
						br_primitives::log_and_capture_simple!(
							error,
							" ❗️ Transaction failed to register ({} address:{}): {}, Error: {}",
							client.get_chain_name(),
							client.address().await,
							metadata,
							err
						);

						client.flush_stalled_transactions().await.unwrap();
					},
				}
			},
			Err(err) => {
				br_primitives::log_and_capture_simple!(
					error,
					" ❗️ Failed to send transaction ({} address:{}): {}, Error: {}",
					client.get_chain_name(),
					client.address().await,
					metadata,
					err
				);

				if err.to_string().to_lowercase().contains("nonce too low") {
					client.flush_stalled_transactions().await.unwrap();
					send_transaction(client, request, requester, metadata, debug_mode, handle);
				}
			},
		}
	});
}

pub mod retry {
	use alloy::{
		rpc::json_rpc::{RequestPacket, ResponsePacket},
		transports::{
			RpcError, TransportError, TransportErrorKind, TransportFut,
			layers::RetryPolicy as RetryPolicyT,
		},
	};
	use std::{
		sync::{
			Arc,
			atomic::{AtomicU32, Ordering},
		},
		task::{Context, Poll},
		time::Duration,
	};
	use tokio::time::sleep;
	use tower::{Layer, Service};

	/// A Transport Layer that is responsible for retrying requests based on the
	/// error type. See [`TransportError`].
	#[derive(Debug, Clone)]
	pub struct RetryBackoffLayer {
		/// The maximum number of retries for errors
		max_retries: u32,
		/// The initial backoff in milliseconds
		initial_backoff: u64,
		/// Chain name
		chain_name: String,
	}

	impl RetryBackoffLayer {
		/// Creates a new retry layer with the given parameters.
		pub const fn new(max_retries: u32, initial_backoff: u64, chain_name: String) -> Self {
			Self { max_retries, initial_backoff, chain_name }
		}
	}

	/// [RetryPolicy] implements [RetryPolicyT] to determine whether to retry depending on err.
	#[derive(Debug, Copy, Clone, Default)]
	#[non_exhaustive]
	pub struct RetryPolicy;

	impl RetryPolicyT for RetryPolicy {
		fn should_retry(&self, _error: &TransportError) -> bool {
			// TODO: Filter out errors that are not retryable. now we retry all errors.
			true
		}

		/// Provides a backoff hint if the error response contains it
		fn backoff_hint(&self, error: &TransportError) -> Option<Duration> {
			if let RpcError::ErrorResp(resp) = error {
				let data = resp.try_data_as::<serde_json::Value>();
				if let Some(Ok(data)) = data {
					// if daily rate limit exceeded, infura returns the requested backoff in the error
					// response
					let backoff_seconds = &data["rate"]["backoff_seconds"];
					// infura rate limit error
					if let Some(seconds) = backoff_seconds.as_u64() {
						return Some(Duration::from_secs(seconds));
					}
					if let Some(seconds) = backoff_seconds.as_f64() {
						return Some(Duration::from_secs(seconds as u64 + 1));
					}
				}
			}

			None
		}
	}

	impl<S> Layer<S> for RetryBackoffLayer {
		type Service = RetryBackoffService<S>;

		fn layer(&self, inner: S) -> Self::Service {
			RetryBackoffService {
				inner,
				policy: RetryPolicy,
				max_retries: self.max_retries,
				initial_backoff: self.initial_backoff,
				requests_enqueued: Arc::new(AtomicU32::new(0)),
				chain_name: self.chain_name.clone(),
			}
		}
	}

	/// A Tower Service used by the [RetryBackoffLayer] that is responsible for retrying all requests on error.
	/// See [TransportError] and [RetryPolicy].
	#[derive(Debug, Clone)]
	pub struct RetryBackoffService<S> {
		/// The inner service
		inner: S,
		/// The retry policy
		policy: RetryPolicy,
		/// The maximum number of retries for errors
		max_retries: u32,
		/// The initial backoff in milliseconds
		initial_backoff: u64,
		/// The number of requests currently enqueued
		requests_enqueued: Arc<AtomicU32>,
		/// Chain name
		chain_name: String,
	}

	impl<S> RetryBackoffService<S> {
		const fn initial_backoff(&self) -> Duration {
			Duration::from_millis(self.initial_backoff)
		}
	}

	impl<S> Service<RequestPacket> for RetryBackoffService<S>
	where
		S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
			+ Send
			+ 'static
			+ Clone,
		S::Future: Send + 'static,
	{
		type Response = ResponsePacket;
		type Error = TransportError;
		type Future = TransportFut<'static>;

		fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			self.inner.poll_ready(cx)
		}

		fn call(&mut self, request: RequestPacket) -> Self::Future {
			let inner = self.inner.clone();
			let this = self.clone();
			let mut inner = std::mem::replace(&mut self.inner, inner);
			Box::pin(async move {
				let _ = this.requests_enqueued.fetch_add(1, Ordering::SeqCst) as u64;
				let mut retry_count: u32 = 0;
				loop {
					let err;
					let res = inner.call(request.clone()).await;
					br_metrics::increase_rpc_calls(&this.chain_name);

					match res {
						Ok(res) => {
							if let Some(e) = res.as_error() {
								err = TransportError::ErrorResp(e.clone())
							} else {
								this.requests_enqueued.fetch_sub(1, Ordering::SeqCst);
								return Ok(res);
							}
						},
						Err(e) => err = e,
					}

					let should_retry = this.policy.should_retry(&err);
					if should_retry {
						retry_count += 1;
						if retry_count > this.max_retries {
							return Err(TransportErrorKind::custom_str(&format!(
								"Max retries exceeded {}",
								err
							)));
						}

						let _ = this.requests_enqueued.load(Ordering::SeqCst) as u64;

						// try to extract the requested backoff from the error or compute the next
						// backoff based on retry count
						let backoff_hint = this.policy.backoff_hint(&err);
						let next_backoff = backoff_hint.unwrap_or_else(|| this.initial_backoff());

						sleep(next_backoff).await;
					} else {
						this.requests_enqueued.fetch_sub(1, Ordering::SeqCst);
						return Err(err);
					}
				}
			})
		}
	}
}
