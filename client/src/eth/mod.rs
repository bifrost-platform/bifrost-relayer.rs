use br_primitives::{
	constants::{
		config::{BOOTSTRAP_BLOCK_OFFSET, NATIVE_BLOCK_TIME},
		errors::{INVALID_CHAIN_ID, PROVIDER_INTERNAL_ERROR},
		tx::DEFAULT_TX_TIMEOUT_MS,
	},
	contracts::{
		authority::BfcStaking::round_meta_data,
		erc20::{Erc20Contract, Erc20Instance},
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
	eips::{BlockNumberOrTag, eip1559::Eip1559Estimation},
	network::{
		BlockResponse, Network, NetworkTransactionBuilder, TransactionBuilder, TransactionResponse,
	},
	primitives::{
		Address, ChainId, FixedBytes, U64, keccak256,
		utils::{Unit, format_units},
	},
	providers::{
		EthCall, PendingTransactionBuilder, Provider, RootProvider, SendableTx, WalletProvider,
		ext::TxPoolApi as _,
		fillers::{FillProvider, TxFiller},
		utils::EIP1559_MIN_PRIORITY_FEE,
	},
	rpc::types::{Filter, Log},
	signers::Signature,
	transports::TransportResult,
};
use eyre::{Result, eyre};
use rand::RngExt;
use sc_service::SpawnTaskHandle;
use std::{
	cmp::max,
	collections::{BTreeMap, HashSet, VecDeque},
	sync::{Arc, Mutex as StdMutex},
	time::Duration,
};
use tokio::{
	sync::{Mutex, RwLock},
	time::sleep,
};
use url::Url;

use crate::failover::FailoverHandle;
use nonce::RelayerNonceManager;

pub mod events;
pub mod failover;
pub mod handlers;
pub mod nonce;
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
	/// The nonce manager shared with the provider's nonce filler.
	nonce_manager: RelayerNonceManager,
	/// The provider failover state of the transport.
	failover: FailoverHandle,
	/// The providers already reported for ignoring `eth_getLogs` filters.
	ignored_filters_reported: Arc<StdMutex<HashSet<String>>>,
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
		nonce_manager: RelayerNonceManager,
		failover: FailoverHandle,
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
			nonce_manager,
			failover,
			ignored_filters_reported: Default::default(),
		}
	}

	/// Get the inner provider.
	pub fn provider(&self) -> Arc<FillProvider<F, P, N>> {
		self.inner.clone()
	}

	/// Get the provider failover state.
	pub fn failover(&self) -> &FailoverHandle {
		&self.failover
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

	/// Flush stalled transactions from the txpool, then re-sync the cached nonce with the chain.
	///
	/// Flushing runs when a transaction stalls or hits `nonce too low`, both of which mean the
	/// cached nonce may have drifted from the chain. Renumbering the txpool entries alone would
	/// leave the cache pointing past them, opening a new gap for the next send.
	pub async fn flush_stalled_transactions(&self) -> Result<()> {
		let _lock = self.martial_law.lock().await;

		let result = self.flush_txpool().await;
		self.nonce_manager.reset().await;
		result
	}

	async fn flush_txpool(&self) -> Result<()> {
		log::info!(target: &self.get_chain_name(), "-[{}] Flushing stalled transactions", sub_display_format(SUB_LOG_TARGET));

		// possibility of txpool being flushed automatically. wait for 2 blocks.
		// also paces callers that resend right after a flush on `nonce too low`, including on
		// chains without the txpool namespace: the nonce is refetched from the chain after every
		// flush, so a lagging node would otherwise turn that resend into a tight loop.
		sleep(Duration::from_millis(self.metadata.call_interval * 2)).await;

		// if txpool namespace is not enabled on the chain, do nothing
		if self.txpool_status().await.is_err() {
			log::info!(
				target: &self.get_chain_name(),
				"-[{}] Txpool namespace is disabled, skipping flush",
				sub_display_format(SUB_LOG_TARGET)
			);
			return Ok(());
		}

		let address = self.address().await;
		let content = match self.txpool_content_from(address).await {
			// some nodes answer `txpool_contentFrom` with an empty result while `txpool_content`
			// holds the account's transactions. the full content is only fetched when the
			// account's pending nonce shows something is actually stuck.
			Ok(content)
				if content.pending.is_empty()
					&& content.queued.is_empty()
					&& self.get_transaction_count(address).pending().await?
						> self.get_transaction_count(address).await? =>
			{
				log::warn!(
					target: &self.get_chain_name(),
					"-[{}] txpool_contentFrom returned nothing for a pending nonce, retrying with full content",
					sub_display_format(SUB_LOG_TARGET),
				);
				self.txpool_content().await?.remove_from(&address)
			},
			Ok(content) => content,
			Err(err) => {
				log::warn!(
					target: &self.get_chain_name(),
					"-[{}] Failed to fetch txpool content, retrying with full content: {}",
					sub_display_format(SUB_LOG_TARGET),
					err
				);
				self.txpool_content().await?.remove_from(&address)
			},
		};
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
					.with_nonce(tx.nonce())
					.with_gas_limit(tx.gas_limit());
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

		// the txpool snapshot may still contain transactions that are already mined (e.g. served by
		// a lagging node behind a load balancer). drop them instead of re-sending their calldata.
		let mut count = self.get_transaction_count(address).await?;
		transactions.retain(|tx| tx.nonce().unwrap() >= count);
		if transactions.is_empty() {
			return Ok(());
		}

		// if the nonce of the first transaction is not equal to the current nonce, update the nonce
		if transactions.front().unwrap().nonce().unwrap() != count {
			for tx in transactions.iter_mut() {
				tx.set_nonce(count);
				count += 1;
			}
		}

		while let Some(mut tx_request) = transactions.pop_front() {
			// the original (or a previously replaced) transaction may have been mined while we were
			// waiting on an earlier one. its nonce is consumed, so there is nothing left to replace.
			let current_nonce = self.get_transaction_count(address).await?;
			if tx_request.nonce().unwrap() < current_nonce {
				log::info!(
					target: &self.get_chain_name(),
					"-[{}] Skipping flush of nonce {} (already mined, current nonce {})",
					sub_display_format(SUB_LOG_TARGET),
					tx_request.nonce().unwrap(),
					current_nonce,
				);
				continue;
			}

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

			let pending = match self.send_transaction(tx_request.clone()).await {
				Ok(pending) => pending,
				// mined between the nonce check above and the submission
				Err(err) if is_nonce_too_low(&err) => {
					log::info!(
						target: &self.get_chain_name(),
						"-[{}] Skipping flush of nonce {} (already mined): {}",
						sub_display_format(SUB_LOG_TARGET),
						tx_request.nonce().unwrap(),
						err,
					);
					continue;
				},
				Err(err) => return Err(err.into()),
			};
			let submitted_tx_hash = *pending.tx_hash();

			match pending
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
					if self.is_already_confirmed(submitted_tx_hash).await {
						log::info!(
							target: &self.get_chain_name(),
							" 🔖 Transaction confirmed ({} tx:{}): Flush (watch() timed out, confirmed via receipt check)",
							self.get_chain_name(),
							submitted_tx_hash,
						);
					} else {
						br_primitives::log_and_capture_simple!(
							error,
							" ❗️ Failed to send transaction ({} address:{}): Flush, Error: {}",
							self.get_chain_name(),
							address,
							err
						);
						transactions.push_front(tx_request);
					}
				},
			}
		}

		log::info!(target: &self.get_chain_name(), "-[{}] Flushing stalled transactions completed", sub_display_format(SUB_LOG_TARGET));

		Ok(())
	}

	/// Checks whether a submitted transaction is already confirmed on-chain via
	/// `eth_getTransactionReceipt`.
	///
	/// Used as a fallback right after a `watch()` timeout: some RPC endpoints (e.g. a
	/// load-balancing proxy in front of multiple backend nodes without sticky sessions) can route
	/// receipt-polling requests to a node lagging behind the one that processed the submitted
	/// transaction, causing a spurious timeout even though the transaction has already confirmed.
	async fn is_already_confirmed(&self, tx_hash: FixedBytes<32>) -> bool {
		matches!(self.get_transaction_receipt(tx_hash).await, Ok(Some(_)))
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
			if !self.metadata.is_native {
				avoid_race_condition().await;
			}

			let gas = self.estimate_gas(request.clone()).await?;
			let coefficient: f64 = GasCoefficient::from(self.metadata.is_native).into();
			// max gas limit for a single transaction is 52,000,000 on bifrost network
			let estimated_gas = (gas as f64 * coefficient).min(52_000_000_f64);

			log::info!(
				target: &self.get_chain_name(),
				"-[{}] 🔍 Estimated gas: {} -> {}",
				sub_display_format(SUB_LOG_TARGET),
				gas, estimated_gas
			);

			request.set_gas_limit(estimated_gas.ceil() as u64);
		}

		if self.metadata.is_native {
			// gas price is fixed to 1000 Gwei on bifrost network
			request.set_max_fee_per_gas(1000 * Unit::GWEI.wei().to::<u128>());
			request.set_max_priority_fee_per_gas(0);
		} else if self.metadata.eip1559 {
			// filled here so the floored estimate below is used instead of `GasFiller`'s own
			let fees = self.estimate_eip1559_fees().await?;
			request.set_max_fee_per_gas(fees.max_fee_per_gas);
			request.set_max_priority_fee_per_gas(fees.max_priority_fee_per_gas);
		} else {
			request.set_gas_price(self.get_gas_price().await?);
		}

		Ok(())
	}

	/// Send a transaction synchronously.
	pub async fn sync_send_transaction(
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
			Ok(pending) => {
				log::info!(
					target: &requester,
					" 🔖 Transaction submitted ({} tx:{}): {}",
					self.get_chain_name(),
					pending.tx_hash(),
					metadata
				);
				pending
			},
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
		let submitted_tx_hash = *pending.tx_hash();

		match pending
			.with_timeout(Some(Duration::from_millis(DEFAULT_TX_TIMEOUT_MS)))
			.watch()
			.await
		{
			Ok(tx_hash) => {
				log::info!(
					target: &requester,
					" ✅ Transaction confirmed ({} tx:{}): {}",
					self.get_chain_name(),
					tx_hash,
					metadata
				);
				Ok(())
			},
			Err(_) => {
				if self.is_already_confirmed(submitted_tx_hash).await {
					log::info!(
						target: &requester,
						" ✅ Transaction confirmed ({} tx:{}): {} (watch() timed out, confirmed via receipt check)",
						self.get_chain_name(),
						submitted_tx_hash,
						metadata
					);
					return Ok(());
				}
				self.flush_stalled_transactions().await?;
				Ok(())
			},
		}
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

	/// Get asset address from Vault contract.
	/// Queries N-Vault first; if the asset is unregistered (remote_asset_index is zero),
	/// falls back to L-Vault (present during the Stage 1/2 migration period).
	pub async fn get_asset_address_by_index(
		&self,
		asset_index_hash: FixedBytes<32>,
		is_inbound: bool,
	) -> Result<Address> {
		let remote_asset_index =
			self.protocol_contracts.vault.remote_asset_pair(asset_index_hash).call().await?;

		let config = if remote_asset_index.is_zero() {
			if let Some(legacy_vault) = &self.protocol_contracts.legacy_vault {
				let legacy_asset_index =
					legacy_vault.remote_asset_pair(asset_index_hash).call().await?;
				legacy_vault.assets_config(legacy_asset_index).call().await?
			} else {
				self.protocol_contracts.vault.assets_config(remote_asset_index).call().await?
			}
		} else {
			self.protocol_contracts.vault.assets_config(remote_asset_index).call().await?
		};

		if is_inbound { Ok(config.unified) } else { Ok(config.target) }
	}

	/// Returns true if a Socket event emitted by L-Socket should be ignored because the asset
	/// has already migrated to the N rail on this chain.
	///
	/// - Outbound: `N-Vault.remote_asset_pair(tokenIDX0) != zero`
	/// - Inbound:  `N-Vault.assets_config(tokenIDX0).unified != zero && .target != zero`
	pub async fn is_asset_migrated_to_n_rail(
		&self,
		msg: &br_primitives::contracts::socket::Socket_Struct::Socket_Message,
		log_address: Address,
		is_inbound: bool,
	) -> Result<bool> {
		let Some(legacy_socket) = &self.protocol_contracts.legacy_socket else {
			return Ok(false);
		};
		if log_address != *legacy_socket.address() {
			return Ok(false);
		}
		if is_inbound {
			let config =
				self.protocol_contracts.vault.assets_config(msg.params.tokenIDX0).call().await?;
			Ok(!config.target.is_zero())
		} else {
			let local_asset = self
				.protocol_contracts
				.vault
				.remote_asset_pair(msg.params.tokenIDX0)
				.call()
				.await?;
			Ok(!local_asset.is_zero())
		}
	}

	/// Determines which socket contract (Legacy or New) currently owns requests for the given
	/// asset on this chain. If this chain has no legacy socket, the single N-Socket address is
	/// always returned.
	///
	/// `treat_as_local` should be `true` when `token_idx0` is this chain's own local asset index
	/// (i.e. this chain is the source of an inbound sequence), and `false` when `token_idx0` is a
	/// remote index that needs mapping via `remote_asset_pair` (i.e. this chain is the destination
	/// of an outbound sequence).
	pub async fn resolve_socket_address(
		&self,
		token_idx0: FixedBytes<32>,
		treat_as_local: bool,
	) -> Result<Address> {
		let Some(legacy_socket) = &self.protocol_contracts.legacy_socket else {
			return Ok(*self.protocol_contracts.socket.address());
		};
		let migrated_to_n = if treat_as_local {
			let config = self.protocol_contracts.vault.assets_config(token_idx0).call().await?;
			!config.target.is_zero()
		} else {
			let remote_asset =
				self.protocol_contracts.vault.remote_asset_pair(token_idx0).call().await?;
			!remote_asset.is_zero()
		};
		Ok(if migrated_to_n {
			*self.protocol_contracts.socket.address()
		} else {
			*legacy_socket.address()
		})
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
		EthCall::gas_estimate(self.inner.weak_client(), tx)
			.block(BlockNumberOrTag::Latest.into())
			.map_resp(|r| r.to::<u64>())
	}

	async fn send_transaction_internal(
		&self,
		tx: SendableTx<N>,
	) -> TransportResult<PendingTransactionBuilder<N>> {
		self.inner.send_transaction_internal(tx).await
	}

	/// alloy falls back to a 1 wei tip when none of the recent blocks paid one (e.g. empty BSC
	/// blocks under a zero base fee), which validators never include and which then blocks every
	/// later nonce. The node's suggested tip is taken instead in that case.
	async fn estimate_eip1559_fees(&self) -> TransportResult<Eip1559Estimation> {
		let fees = self.inner.estimate_eip1559_fees().await?;
		if fees.max_priority_fee_per_gas > EIP1559_MIN_PRIORITY_FEE {
			return Ok(fees);
		}
		Ok(with_tip(fees, self.get_max_priority_fee_per_gas().await?))
	}

	/// Keeps only the logs that match `filter`, which callers trust: some providers do not apply
	/// it (QuickNode answers a single-block range with every log of the block).
	async fn get_logs(&self, filter: &Filter) -> TransportResult<Vec<Log>> {
		let (logs, dropped) = retain_matching(filter, self.inner.get_logs(filter).await?);
		if dropped > 0 {
			let provider = self.failover.current_url().to_string();
			if self.ignored_filters_reported.lock().unwrap().insert(provider.clone()) {
				br_primitives::log_and_capture_simple!(
					warn,
					"-[{}] ⚠️  [{}] Provider {} ignores eth_getLogs filters: dropped {} log(s) out of the filter",
					sub_display_format(SUB_LOG_TARGET),
					self.get_chain_name(),
					provider,
					dropped,
				);
			}
		}
		Ok(logs)
	}
}

/// `fees` with the priority fee raised to `tip` (never lowered), keeping the base fee allowance.
fn with_tip(fees: Eip1559Estimation, tip: u128) -> Eip1559Estimation {
	let tip = tip.max(fees.max_priority_fee_per_gas);
	Eip1559Estimation {
		max_fee_per_gas: fees.max_fee_per_gas - fees.max_priority_fee_per_gas + tip,
		max_priority_fee_per_gas: tip,
	}
}

/// The logs that match `filter` (address, topics, block range, not removed), and how many did
/// not.
fn retain_matching(filter: &Filter, logs: Vec<Log>) -> (Vec<Log>, usize) {
	let total = logs.len();
	let logs: Vec<Log> =
		logs.into_iter().filter(|log| !log.removed && filter.rpc_matches(log)).collect();
	let dropped = total - logs.len();
	(logs, dropped)
}

/// Whether the error is a `nonce too low` rejection, i.e. the nonce is already consumed on-chain.
pub fn is_nonce_too_low(err: &impl std::fmt::Display) -> bool {
	err.to_string().to_lowercase().contains("nonce too low")
}

pub async fn avoid_race_condition() {
	// to avoid duplicate(will revert) external networks transactions
	let duration = Duration::from_millis(rand::rng().random_range(0..=12000));
	sleep(duration).await;
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
				let err_string = err.to_string();

				if err_string.contains("revert tx already executed") {
					return;
				}

				let msg = format!(
					" ❗️ Failed to estimate gas ({} address:{}): {}, Error: {}, Request: {:?}",
					client.get_chain_name(),
					client.address().await,
					metadata,
					err,
					request
				);
				log::error!(target: &requester, "{msg}");

				const SENTRY_IGNORE_PATTERNS: [&str; 6] = [
					// roundup already executed on external chain through round_control_relay()
					"latest round",
					// outbound aggregated relay already executed
					"_outbound_exec_relay status",
					// outbound aggregated relay already reverted
					"_outbound_revert_relay status",
					// inbound aggregated relay already executed
					"phase3",
					// bridge request already committed
					"revert poll filtered",
					// Hook execution already executed
					"Already processed",
				];
				if !SENTRY_IGNORE_PATTERNS.iter().any(|pattern| err_string.contains(pattern)) {
					sentry::capture_message(&msg, sentry::Level::Error);
				}
			}
			return;
		}

		match client.send_transaction(request.clone()).await {
			Ok(pending) => {
				log::info!(
					target: &requester,
					" 🔖 Transaction submitted ({} tx:{}): {}",
					client.get_chain_name(),
					pending.tx_hash(),
					metadata
				);
				let submitted_tx_hash = *pending.tx_hash();

				match pending
					.with_timeout(Some(Duration::from_millis(DEFAULT_TX_TIMEOUT_MS)))
					.watch()
					.await
				{
					Ok(tx_hash) => {
						log::info!(
							target: &requester,
							" ✅ Transaction confirmed ({} tx:{}): {}",
							client.get_chain_name(),
							tx_hash,
							metadata
						);
					},
					Err(err) => {
						if client.is_already_confirmed(submitted_tx_hash).await {
							log::info!(
								target: &requester,
								" ✅ Transaction confirmed ({} tx:{}): {} (watch() timed out, confirmed via receipt check)",
								client.get_chain_name(),
								submitted_tx_hash,
								metadata
							);
						} else {
							br_primitives::log_and_capture_simple!(
								error,
								" ❗️ Transaction failed to register ({} address:{}): {}, Error: {}, Request: {:?}",
								client.get_chain_name(),
								client.address().await,
								metadata,
								err,
								request
							);

							if let Err(err) = client.flush_stalled_transactions().await {
								br_primitives::log_and_capture_simple!(
									error,
									" ❗️ Failed to flush stalled transactions ({}): {}",
									client.get_chain_name(),
									err
								);
							}
						}
					},
				}
			},
			Err(err) => {
				br_primitives::log_and_capture_simple!(
					error,
					" ❗️ Failed to send transaction ({} address:{}): {}, Error: {}, Request: {:?}",
					client.get_chain_name(),
					client.address().await,
					metadata,
					err,
					request
				);

				if is_nonce_too_low(&err) {
					if let Err(err) = client.flush_stalled_transactions().await {
						br_primitives::log_and_capture_simple!(
							error,
							" ❗️ Failed to flush stalled transactions ({}): {}",
							client.get_chain_name(),
							err
						);
					}
					send_transaction(client, request, requester, metadata, debug_mode, handle);
				}
			},
		}
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloy::primitives::address;

	fn log(address: &str, block: u64, removed: bool) -> Log {
		serde_json::from_value(serde_json::json!({
			"address": address,
			"topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
			"data": "0x",
			"blockNumber": format!("{block:#x}"),
			"blockHash": "0x0000000000000000000000000000000000000000000000000000000000000001",
			"transactionHash": "0x0000000000000000000000000000000000000000000000000000000000000002",
			"transactionIndex": "0x0",
			"logIndex": "0x0",
			"removed": removed,
		}))
		.unwrap()
	}

	#[test]
	fn logs_out_of_the_filter_are_dropped() {
		let socket = "0x24d0fff5601e50647f791607b1c2b6c1e0360326";
		let filter = Filter::new()
			.address(address!("0x24d0fff5601e50647f791607b1c2b6c1e0360326"))
			.from_block(100u64)
			.to_block(100u64);
		let logs = vec![
			log(socket, 100, false),
			// another contract's log, as a provider ignoring the filter returns it
			log("0x4200000000000000000000000000000000000006", 100, false),
			// out of the range
			log(socket, 101, false),
			log(socket, 100, true),
		];
		let (kept, dropped) = retain_matching(&filter, logs);
		assert_eq!(kept.len(), 1);
		assert_eq!(dropped, 3);
		assert_eq!(kept[0].address(), address!("0x24d0fff5601e50647f791607b1c2b6c1e0360326"));
	}

	#[test]
	fn tip_is_raised_to_the_suggested_one() {
		let fees = |max_fee_per_gas, max_priority_fee_per_gas| Eip1559Estimation {
			max_fee_per_gas,
			max_priority_fee_per_gas,
		};
		// zero base fee (BSC): the 1 wei fallback becomes the node's suggestion
		assert_eq!(with_tip(fees(1, 1), 100_000_000), fees(100_000_000, 100_000_000));
		// the base fee allowance is kept
		assert_eq!(with_tip(fees(2_000_000_001, 1), 5), fees(2_000_000_005, 5));
		// a lower suggestion (Bifrost answers 0) never lowers the tip
		assert_eq!(with_tip(fees(10, 1), 0), fees(10, 1));
	}
}
