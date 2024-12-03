use std::{str::FromStr, sync::Arc};

use alloy::{
	network::Ethereum,
	primitives::{Address, ChainId},
	providers::{
		fillers::{FillProvider, TxFiller},
		Provider, WalletProvider,
	},
	rpc::types::TransactionRequest,
	signers::Signature,
	transports::Transport,
};
use url::Url;

use crate::{
	constants::errors::{INVALID_CONTRACT_ADDRESS, MISSING_CONTRACT_ADDRESS},
	contracts::{
		authority::AuthorityContract::{self, AuthorityContractInstance},
		bitcoin_socket::BitcoinSocketContract::{self, BitcoinSocketContractInstance},
		chainlink_aggregator::ChainlinkContract::{self, ChainlinkContractInstance},
		registration_pool::RegistrationPoolContract::{self, RegistrationPoolContractInstance},
		relay_executive::RelayExecutiveContract::{self, RelayExecutiveContractInstance},
		relayer_manager::RelayerManagerContract::{self, RelayerManagerContractInstance},
		socket::SocketContract::{self, SocketContractInstance},
		socket_queue::SocketQueueContract::{self, SocketQueueContractInstance},
	},
};

#[derive(Clone)]
/// The metadata of the EVM provider.
pub struct ProviderMetadata {
	/// The name of this provider.
	pub name: String,
	/// The provider URL. (Allowed values: `http`, `https`)
	pub url: Url,
	/// Id of chain which this client interact with.
	pub id: ChainId,
	/// The bitcoin chain ID used for CCCP.
	pub bitcoin_chain_id: Option<ChainId>,
	/// The total number of confirmations required for a block to be processed. (block
	/// confirmations + eth_getLogs batch size)
	pub block_confirmations: u64,
	/// The batch size used on `eth_getLogs()` requests.
	pub get_logs_batch_size: u64,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// Relay direction when CCCP event points this chain as destination.
	pub if_destination_chain: RelayDirection,
	/// The flag whether the chain is Bifrost(native) or an external chain.
	pub is_native: bool,
}

impl ProviderMetadata {
	pub fn new(
		name: String,
		url: Url,
		id: ChainId,
		bitcoin_chain_id: Option<ChainId>,
		block_confirmations: u64,
		call_interval: u64,
		get_logs_batch_size: u64,
		is_native: bool,
	) -> Self {
		Self {
			name,
			url,
			id,
			bitcoin_chain_id,
			block_confirmations: block_confirmations.saturating_add(get_logs_batch_size),
			get_logs_batch_size,
			call_interval,
			is_native,
			if_destination_chain: match is_native {
				true => RelayDirection::Inbound,
				false => RelayDirection::Outbound,
			},
		}
	}
}

#[derive(Clone)]
pub struct AggregatorContracts<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// Chainlink usdc/usd aggregator
	pub chainlink_usdc_usd:
		Option<ChainlinkContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// Chainlink usdt/usd aggregator
	pub chainlink_usdt_usd:
		Option<ChainlinkContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// Chainlink dai/usd aggregator
	pub chainlink_dai_usd:
		Option<ChainlinkContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// Chainlink btc/usd aggregator
	pub chainlink_btc_usd:
		Option<ChainlinkContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// Chainlink wbtc/usd aggregator
	pub chainlink_wbtc_usd:
		Option<ChainlinkContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// Chainlink cbbtc/usd aggregator
	pub chainlink_cbbtc_usd:
		Option<ChainlinkContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
}

impl<F, P, T> AggregatorContracts<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	pub fn new(
		provider: Arc<FillProvider<F, P, T, Ethereum>>,
		chainlink_usdc_usd_address: Option<String>,
		chainlink_usdt_usd_address: Option<String>,
		chainlink_dai_usd_address: Option<String>,
		chainlink_btc_usd_address: Option<String>,
		chainlink_wbtc_usd_address: Option<String>,
		chainlink_cbbtc_usd_address: Option<String>,
	) -> Self {
		let create_contract_instance = |address: String| {
			ChainlinkContract::new(
				Address::from_str(&address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			)
		};

		Self {
			chainlink_usdc_usd: chainlink_usdc_usd_address.map(create_contract_instance),
			chainlink_usdt_usd: chainlink_usdt_usd_address.map(create_contract_instance),
			chainlink_dai_usd: chainlink_dai_usd_address.map(create_contract_instance),
			chainlink_btc_usd: chainlink_btc_usd_address.map(create_contract_instance),
			chainlink_wbtc_usd: chainlink_wbtc_usd_address.map(create_contract_instance),
			chainlink_cbbtc_usd: chainlink_cbbtc_usd_address.map(create_contract_instance),
		}
	}
}

impl<F, P, T> Default for AggregatorContracts<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	fn default() -> Self {
		Self {
			chainlink_usdc_usd: None,
			chainlink_usdt_usd: None,
			chainlink_dai_usd: None,
			chainlink_btc_usd: None,
			chainlink_wbtc_usd: None,
			chainlink_cbbtc_usd: None,
		}
	}
}

#[derive(Clone)]
/// The protocol contract instances of the EVM provider.
pub struct ProtocolContracts<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// SocketContract
	pub socket: SocketContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>,
	/// AuthorityContract
	pub authority: AuthorityContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>,
	/// RelayerManagerContract (Bifrost only)
	pub relayer_manager:
		Option<RelayerManagerContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// BitcoinSocketContract (Bifrost only)
	pub bitcoin_socket:
		Option<BitcoinSocketContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// SocketQueueContract (Bifrost only)
	pub socket_queue: Option<SocketQueueContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// RegistrationPoolContract (Bifrost only)
	pub registration_pool:
		Option<RegistrationPoolContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
	/// RelayExecutiveContract (Bifrost only)
	pub relay_executive:
		Option<RelayExecutiveContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>>>,
}

impl<F, P, T> ProtocolContracts<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	pub fn new(
		is_native: bool,
		provider: Arc<FillProvider<F, P, T, Ethereum>>,
		socket_address: String,
		authority_address: String,
		relayer_manager_address: Option<String>,
		bitcoin_socket_address: Option<String>,
		socket_queue_address: Option<String>,
		registration_pool_address: Option<String>,
		relay_executive_address: Option<String>,
	) -> Self {
		let mut contracts = Self {
			socket: SocketContract::new(
				Address::from_str(&socket_address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			),
			authority: AuthorityContract::new(
				Address::from_str(&authority_address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			),
			relayer_manager: None,
			bitcoin_socket: None,
			socket_queue: None,
			registration_pool: None,
			relay_executive: None,
		};
		if is_native {
			contracts.relayer_manager = Some(RelayerManagerContract::new(
				Address::from_str(&relayer_manager_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.bitcoin_socket = Some(BitcoinSocketContract::new(
				Address::from_str(&bitcoin_socket_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.socket_queue = Some(SocketQueueContract::new(
				Address::from_str(&socket_queue_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.registration_pool = Some(RegistrationPoolContract::new(
				Address::from_str(&registration_pool_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.relay_executive = Some(RelayExecutiveContract::new(
				Address::from_str(&relay_executive_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
		}
		contracts
	}
}

#[derive(Clone, Debug)]
/// Coefficients to multiply the estimated gas amount.
pub enum GasCoefficient {
	/// The lowest coefficient. Only used on transaction submissions to external chains.
	Low,
	/// The medium coefficient. Only used on transaction submissions to Bifrost.
	Mid,
	/// The high coefficient. Currently not in used.
	High,
}

impl GasCoefficient {
	pub fn into_f64(&self) -> f64 {
		f64::from(self)
	}
}

impl From<&GasCoefficient> for f64 {
	fn from(value: &GasCoefficient) -> Self {
		match value {
			GasCoefficient::Low => 1.2,
			GasCoefficient::Mid => 7.0,
			GasCoefficient::High => 10.0,
		}
	}
}

#[derive(Clone, Copy, Debug)]
/// The roundup event status.
pub enum RoundUpEventStatus {
	/// A single relayer has relayed a `RoundUp` event, however the quorum wasn't reached yet.
	NextAuthorityRelayed = 9,
	/// A single relayer has relayed a `RoundUp` event and the quorum has been reached.
	NextAuthorityCommitted,
}

impl RoundUpEventStatus {
	pub fn from_u8(status: u8) -> Self {
		match status {
			9 => RoundUpEventStatus::NextAuthorityRelayed,
			10 => RoundUpEventStatus::NextAuthorityCommitted,
			_ => panic!("Unknown roundup event status received: {:?}", status),
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
/// The socket event status.
pub enum SocketEventStatus {
	/// When the `SocketMessage` with given request ID does not exist
	/// on a certain chain, the status will be `None`.
	None,
	/// A bridge request has been successfully initialized on the source chain.
	Requested,
	/// The opposite side of `Requested`.
	Failed,
	/// A bridge request has been successfully executed on the destination chain.
	Executed,
	/// The opposite side of `Executed`.
	Reverted,
	/// A bridge request has been accepted on Bifrost.
	Accepted,
	/// The opposite side of `Accepted`.
	Rejected,
	/// A bridge request has been successfully committed back to the source chain.
	Committed,
	/// The opposite side of `Committed`.
	/// The bridged asset will be refunded to the user.
	Rollbacked,
}

impl From<u8> for SocketEventStatus {
	fn from(status: u8) -> Self {
		match status {
			0 => SocketEventStatus::None,
			1 => SocketEventStatus::Requested,
			2 => SocketEventStatus::Failed,
			3 => SocketEventStatus::Executed,
			4 => SocketEventStatus::Reverted,
			5 => SocketEventStatus::Accepted,
			6 => SocketEventStatus::Rejected,
			7 => SocketEventStatus::Committed,
			8 => SocketEventStatus::Rollbacked,
			_ => panic!("Unknown socket event status received: {:?}", status),
		}
	}
}

impl From<&u8> for SocketEventStatus {
	fn from(value: &u8) -> Self {
		Self::from(*value)
	}
}

impl From<SocketEventStatus> for u8 {
	fn from(status: SocketEventStatus) -> Self {
		match status {
			SocketEventStatus::None => 0,
			SocketEventStatus::Requested => 1,
			SocketEventStatus::Failed => 2,
			SocketEventStatus::Executed => 3,
			SocketEventStatus::Reverted => 4,
			SocketEventStatus::Accepted => 5,
			SocketEventStatus::Rejected => 6,
			SocketEventStatus::Committed => 7,
			SocketEventStatus::Rollbacked => 8,
		}
	}
}

#[derive(Clone, Copy, Debug)]
/// The CCCP protocols relay direction.
pub enum RelayDirection {
	/// From external network, to bifrost network.
	Inbound,
	/// From bifrost network, to external network.
	Outbound,
}

#[derive(Clone, Debug, PartialEq)]
/// The state for bootstrapping
pub enum BootstrapState {
	/// phase 0. check if the node is in syncing
	NodeSyncing,
	/// phase 1-1. emit all pushed RoundUp event
	BootstrapRoundUpPhase1,
	/// phase 1-2. bootstrap for RoundUp event
	BootstrapRoundUpPhase2,
	/// phase 2. bootstrap for Socket event
	BootstrapSocketRelay,
	/// phase 3. process for latest block as normal
	NormalStart,
}

#[derive(Clone, Debug)]
/// The information of a recovered signature.
pub struct RecoveredSignature {
	/// The original index that represents the order from the result of `get_signatures()`.
	pub idx: usize,
	/// The signature of the message.
	pub signature: Signature,
	/// The account who signed the message.
	pub signer: Address,
}

impl RecoveredSignature {
	pub fn new(idx: usize, signature: Signature, signer: Address) -> Self {
		Self { idx, signature, signer }
	}
}

#[derive(Clone, Debug)]
/// The built relay transaction request.
pub struct BuiltRelayTransaction {
	/// The raw transaction request body.
	pub tx_request: TransactionRequest,
	/// The flag whether if the destination is an external chain.
	pub is_external: bool,
}

impl BuiltRelayTransaction {
	pub fn new(tx_request: TransactionRequest, is_external: bool) -> Self {
		Self { tx_request, is_external }
	}
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
	}

	impl RetryBackoffLayer {
		/// Creates a new retry layer with the given parameters.
		pub const fn new(max_retries: u32, initial_backoff: u64) -> Self {
			Self { max_retries, initial_backoff }
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
