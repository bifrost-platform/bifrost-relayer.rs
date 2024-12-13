use br_primitives::{
	constants::{
		config::{BOOTSTRAP_BLOCK_OFFSET, NATIVE_BLOCK_TIME},
		errors::{INSUFFICIENT_FUNDS, INVALID_CHAIN_ID, PROVIDER_INTERNAL_ERROR},
	},
	contracts::authority::BfcStaking::round_meta_data,
	eth::{AggregatorContracts, GasCoefficient, ProtocolContracts, ProviderMetadata},
	tx::TxRequestMetadata,
	utils::generate_delay,
};

use alloy::{
	consensus::Transaction,
	network::{AnyNetwork, AnyRpcTransaction, AnyTypedTransaction, TransactionResponse as _},
	primitives::{
		utils::{format_units, parse_ether, Unit},
		Address, ChainId,
	},
	providers::{
		ext::TxPoolApi as _,
		fillers::{FillProvider, TxFiller},
		PendingTransactionBuilder, Provider, RootProvider, SendableTx, WalletProvider,
	},
	rpc::types::{serde_helpers::WithOtherFields, txpool::TxpoolContent, TransactionRequest},
	signers::{local::LocalSigner, Signature},
	transports::{Transport, TransportResult},
};
use eyre::{eyre, Result};
use k256::ecdsa::SigningKey;
use sc_service::SpawnTaskHandle;
use sha3::{Digest, Keccak256};
use std::{cmp::max, collections::VecDeque, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use url::Url;

pub mod events;
pub mod handlers;
pub mod traits;

#[derive(Clone)]
pub struct EthClient<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The inner provider.
	inner: Arc<FillProvider<F, P, T, AnyNetwork>>,
	/// The signer.
	pub signer: LocalSigner<SigningKey>,
	/// The provider metadata.
	pub metadata: ProviderMetadata,
	/// The protocol contracts.
	pub protocol_contracts: ProtocolContracts<F, P, T>,
	/// The aggregator contracts.
	pub aggregator_contracts: AggregatorContracts<F, P, T>,
	/// flushing not allowed to work concurrently.
	pub martial_law: Arc<Mutex<()>>,
}

impl<F, P, T> EthClient<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Create a new EthClient
	pub fn new(
		inner: Arc<FillProvider<F, P, T, AnyNetwork>>,
		signer: LocalSigner<SigningKey>,
		metadata: ProviderMetadata,
		protocol_contracts: ProtocolContracts<F, P, T>,
		aggregator_contracts: AggregatorContracts<F, P, T>,
	) -> Self {
		Self {
			inner,
			signer,
			metadata,
			protocol_contracts,
			aggregator_contracts,
			martial_law: Arc::new(Mutex::new(())),
		}
	}

	/// Verifies whether the configured chain id and the provider's chain id match.
	pub async fn verify_chain_id(&self) -> Result<()> {
		let chain_id = self.get_chain_id().await?;
		if chain_id != self.metadata.id {
			Err(eyre!(INVALID_CHAIN_ID))
		} else {
			Ok(())
		}
	}

	/// Verifies whether the relayer has enough balance to pay for the transaction fees.
	pub async fn verify_minimum_balance(&self) -> Result<()> {
		if self.metadata.is_native {
			let balance = self.get_balance(self.address()).await?;
			if balance < parse_ether("1")? {
				eyre::bail!(INSUFFICIENT_FUNDS)
			}
		}
		Ok(())
	}

	/// Get the signer address.
	pub fn address(&self) -> Address {
		self.inner.default_signer_address()
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
			format_units(self.get_balance(self.address()).await?, "ether")?.parse::<f64>()?,
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
		let cred = self.signer.credential();
		Ok(Signature::from(cred.sign_digest_recoverable(Keccak256::new_with_prefix(message))?))
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

		let current_block = self.get_block(block_number.into(), true.into()).await?;
		let prev_block = self.get_block(prev_block_number.into(), true.into()).await?;
		if let (Some(current_block), Some(prev_block)) = (current_block, prev_block) {
			let current_timestamp = current_block.header.timestamp;
			let prev_timestamp = prev_block.header.timestamp;
			let timestamp_diff = current_timestamp.checked_sub(prev_timestamp).unwrap() as f64;
			let avg_block_time = timestamp_diff / block_diff as f64;

			let blocks = round_offset
				.checked_mul(round_info.round_length.saturating_to::<u64>())
				.unwrap();
			let blocks_to_native_chain_time = blocks.checked_mul(NATIVE_BLOCK_TIME).unwrap();
			let bootstrap_offset_height = blocks_to_native_chain_time as f64 / avg_block_time;
			Ok(bootstrap_offset_height.ceil() as u64)
		} else {
			Err(eyre!(PROVIDER_INTERNAL_ERROR))
		}
	}

	/// Verifies whether the current relayer was selected at the current round
	pub async fn is_selected_relayer(&self) -> Result<bool> {
		let relayer_manager = self.protocol_contracts.relayer_manager.as_ref().unwrap();
		Ok(relayer_manager.is_selected_relayer(self.address(), false).call().await?._0)
	}

	/// Flush stalled transactions from the txpool.
	pub async fn flush_stalled_transactions(&self) -> Result<()> {
		let _lock = self.martial_law.lock().await;

		// if the chain is native or txpool is not enabled, do nothing
		if self.metadata.is_native || self.txpool_status().await.is_err() {
			return Ok(());
		}

		// possibility of txpool being flushed automatically. wait for 2 blocks.
		tokio::time::sleep(Duration::from_millis(self.metadata.call_interval * 2)).await;

		let mut content: TxpoolContent<AnyRpcTransaction> = self.txpool_content().await?;
		let pending = content.remove_from(&self.address()).pending;
		let mut transactions = pending.into_iter().map(|(_, tx)| tx).collect::<VecDeque<_>>();
		transactions.make_contiguous().sort_by(|a, b| a.nonce().cmp(&b.nonce()));

		while let Some(tx) = transactions.pop_front() {
			if self.get_transaction_receipt(tx.tx_hash()).await.unwrap().is_some() {
				continue;
			}

			let mut tx_request = WithOtherFields::<TransactionRequest>::from(
				AnyTypedTransaction::from(tx.inner.inner.clone()),
			);

			// RBF
			if tx.is_legacy_gas() {
				let new_gas_price = ((tx_request.gas_price.unwrap() as f64) * 1.1).ceil() as u128;
				let current_gas_price = self.get_gas_price().await.unwrap();

				tx_request.gas_price = Some(max(new_gas_price, current_gas_price));
			} else {
				let current_gas_price = self.estimate_eip1559_fees(None).await.unwrap();

				let new_max_fee_per_gas =
					(tx_request.max_fee_per_gas.unwrap() as f64 * 1.1).ceil() as u128;
				let new_max_priority_fee_per_gas =
					(tx_request.max_priority_fee_per_gas.unwrap() as f64 * 1.1).ceil() as u128;

				tx_request.max_fee_per_gas =
					Some(max(new_max_fee_per_gas, current_gas_price.max_fee_per_gas));
				tx_request.max_priority_fee_per_gas = Some(max(
					new_max_priority_fee_per_gas,
					current_gas_price.max_priority_fee_per_gas,
				));
			}

			let pending = self
				.send_transaction(tx_request)
				.await?
				.with_timeout(Some(Duration::from_millis(self.metadata.call_interval)));
			if pending.watch().await.is_err() {
				transactions.push_front(tx);
			}
		}

		Ok(())
	}
}

#[async_trait::async_trait]
impl<F, P, T> Provider<T, AnyNetwork> for EthClient<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	fn root(&self) -> &RootProvider<T, AnyNetwork> {
		self.inner.root()
	}

	async fn send_transaction_internal(
		&self,
		tx: SendableTx<AnyNetwork>,
	) -> TransportResult<PendingTransactionBuilder<T, AnyNetwork>> {
		self.inner.send_transaction_internal(tx).await
	}
}

pub fn send_transaction<F, P, T>(
	client: Arc<EthClient<F, P, T>>,
	mut request: TransactionRequest,
	requester: String,
	metadata: TxRequestMetadata,
	handle: SpawnTaskHandle,
) where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	let this_handle = handle.clone();
	this_handle.spawn("send_transaction", None, async move {
		request.from = Some(client.address());

		match client.estimate_gas(&WithOtherFields::new(request.clone())).await {
			Ok(gas) => {
				let coefficient: f64 = GasCoefficient::from(client.metadata.is_native).into();
				let estimated_gas = gas as f64 * coefficient;
				request.gas = Some(estimated_gas.ceil() as u64);
			},
			Err(err) => {
				let msg = format!(
					" ❗️ Failed to estimate gas ({} address:{}): {}, Error: {}",
					client.get_chain_name(),
					client.address(),
					metadata,
					err
				);
				log::error!(target: &requester, "{msg}");
				sentry::capture_message(&msg, sentry::Level::Error);
				return;
			},
		};

		if client.metadata.is_native {
			// gas price is fixed to 1000 Gwei on bifrost network
			request.max_fee_per_gas = Some(1000 * Unit::GWEI.wei().to::<u128>());
			request.max_priority_fee_per_gas = Some(0);
		} else {
			// to avoid duplicate(will revert) external networks transactions
			tokio::time::sleep(Duration::from_millis(generate_delay())).await;

			if !client.metadata.eip1559 {
				request.gas_price = Some(client.get_gas_price().await.unwrap());
			}
		}

		match client.send_transaction(WithOtherFields::new(request.clone())).await {
			Ok(pending) => {
				log::info!(
					target: &requester,
					" 🔖 Send transaction ({} tx:{}): {}",
					client.get_chain_name(),
					pending.tx_hash(),
					metadata
				);
			},
			Err(err) => {
				let msg = format!(
					" ❗️ Failed to send transaction ({} address:{}): {}, Error: {}",
					client.get_chain_name(),
					client.address(),
					metadata,
					err
				);
				log::error!(target: &requester, "{msg}");
				sentry::capture_message(&msg, sentry::Level::Error);

				if err.to_string().to_lowercase().contains("nonce too low") {
					client.flush_stalled_transactions().await.unwrap();
					send_transaction(client, request, requester, metadata, handle);
				}
			},
		}
	});
}

pub mod retry {
	use alloy::{
		rpc::json_rpc::{RequestPacket, ResponsePacket},
		transports::{
			layers::RetryPolicy as RetryPolicyT, RpcError, TransportError, TransportErrorKind,
			TransportFut,
		},
	};
	use std::{
		sync::{
			atomic::{AtomicU32, Ordering},
			Arc,
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

	/// [RetryPolicy] implements [RetryPolicyT] to determine whether to retry depending on the
	/// err.
	#[derive(Debug, Copy, Clone, Default)]
	#[non_exhaustive]
	pub struct RetryPolicy;

	impl RetryPolicyT for RetryPolicy {
		fn should_retry(&self, _error: &TransportError) -> bool {
			// TODO: Filter out errors that are not retryable. now we retry all errors.
			true
		}

		/// Provides a backoff hint if the error response contains it
		fn backoff_hint(&self, error: &TransportError) -> Option<std::time::Duration> {
			if let RpcError::ErrorResp(resp) = error {
				let data = resp.try_data_as::<serde_json::Value>();
				if let Some(Ok(data)) = data {
					// if daily rate limit exceeded, infura returns the requested backoff in the error
					// response
					let backoff_seconds = &data["rate"]["backoff_seconds"];
					// infura rate limit error
					if let Some(seconds) = backoff_seconds.as_u64() {
						return Some(std::time::Duration::from_secs(seconds));
					}
					if let Some(seconds) = backoff_seconds.as_f64() {
						return Some(std::time::Duration::from_secs(seconds as u64 + 1));
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
