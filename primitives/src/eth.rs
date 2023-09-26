use std::{str::FromStr, sync::Arc};

use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{Address, Signature, TransactionRequest, Uint8, H160, U64},
};
use url::Url;

use crate::{
	constants::errors::{INVALID_CONTRACT_ADDRESS, INVALID_PROVIDER_URL, MISSING_CONTRACT_ADDRESS},
	contracts::{
		authority::AuthorityContract, bitcoin_socket::BitcoinSocketContract,
		chainlink_aggregator::ChainlinkContract, registration_pool::RegistrationPoolCoㄴract,
		relay_executive::RelayExecutiveContract, relayer_manager::RelayerManagerContract,
		socket::SocketContract, socket_queue::SocketQueueContract,
	},
};

/// The type of EVM chain ID's.
pub type ChainID = u32;

/// The metadata of the EVM provider.
pub struct ProviderMetadata {
	/// The name of this provider.
	pub name: String,
	/// The provider URL. (Allowed values: `http`, `https`)
	pub url: Url,
	/// Id of chain which this client interact with.
	pub id: ChainID,
	/// The bitcoin chain ID used for CCCP.
	pub bitcoin_chain_id: Option<ChainID>,
	/// The total number of confirmations required for a block to be processed. (block
	/// confirmations + eth_getLogs batch size)
	pub block_confirmations: U64,
	/// The batch size used on `eth_getLogs()` requests.
	pub get_logs_batch_size: U64,
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
		url: String,
		id: ChainID,
		bitcoin_chain_id: Option<ChainID>,
		block_confirmations: u64,
		call_interval: u64,
		get_logs_batch_size: u64,
		is_native: bool,
	) -> Self {
		Self {
			name,
			url: Url::parse(&url).expect(INVALID_PROVIDER_URL),
			id,
			bitcoin_chain_id,
			block_confirmations: U64::from(block_confirmations.saturating_add(get_logs_batch_size)),
			get_logs_batch_size: U64::from(get_logs_batch_size),
			call_interval,
			is_native,
			if_destination_chain: match is_native {
				true => RelayDirection::Inbound,
				false => RelayDirection::Outbound,
			},
		}
	}
}

pub struct AggregatorContracts<T> {
	/// Chainlink usdc/usd aggregator
	pub chainlink_usdc_usd: Option<ChainlinkContract<Provider<T>>>,
	/// Chainlink usdt/usd aggregator
	pub chainlink_usdt_usd: Option<ChainlinkContract<Provider<T>>>,
	/// Chainlink dai/usd aggregator
	pub chainlink_dai_usd: Option<ChainlinkContract<Provider<T>>>,
	/// Chainlink btc/usd aggregator
	pub chainlink_btc_usd: Option<ChainlinkContract<Provider<T>>>,
	/// Chainlink wbtc/usd aggregator
	pub chainlink_wbtc_usd: Option<ChainlinkContract<Provider<T>>>,
}

impl<T: JsonRpcClient> AggregatorContracts<T> {
	pub fn new(
		provider: Arc<Provider<T>>,
		chainlink_usdc_usd_address: Option<String>,
		chainlink_usdt_usd_address: Option<String>,
		chainlink_dai_usd_address: Option<String>,
		chainlink_btc_usd_address: Option<String>,
		chainlink_wbtc_usd_address: Option<String>,
	) -> Self {
		let create_contract_instance = |address: String| {
			ChainlinkContract::new(
				H160::from_str(&address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			)
		};

		Self {
			chainlink_usdc_usd: chainlink_usdc_usd_address.map(create_contract_instance),
			chainlink_usdt_usd: chainlink_usdt_usd_address.map(create_contract_instance),
			chainlink_dai_usd: chainlink_dai_usd_address.map(create_contract_instance),
			chainlink_btc_usd: chainlink_btc_usd_address.map(create_contract_instance),
			chainlink_wbtc_usd: chainlink_wbtc_usd_address.map(create_contract_instance),
		}
	}
}

impl<T: JsonRpcClient> Default for AggregatorContracts<T> {
	fn default() -> Self {
		Self {
			chainlink_usdc_usd: None,
			chainlink_usdt_usd: None,
			chainlink_dai_usd: None,
			chainlink_btc_usd: None,
			chainlink_wbtc_usd: None,
		}
	}
}

/// The protocol contract instances of the EVM provider.
pub struct ProtocolContracts<T> {
	/// SocketContract
	pub socket: SocketContract<Provider<T>>,
	/// AuthorityContract
	pub authority: AuthorityContract<Provider<T>>,
	/// RelayerManagerContract (Bifrost only)
	pub relayer_manager: Option<RelayerManagerContract<Provider<T>>>,
	/// BitcoinSocketContract (Bifrost only)
	pub bitcoin_socket: Option<BitcoinSocketContract<Provider<T>>>,
	/// SocketQueueContract (Bifrost only)
	pub socket_queue: Option<SocketQueueContract<Provider<T>>>,
	/// RegistrationPoolContract (Bifrost only)
	pub registration_pool: Option<RegistrationPoolContract<Provider<T>>>,
	/// RelayExecutiveContract (Bifrost only)
	pub relay_executive: Option<RelayExecutiveContract<Provider<T>>>,
}

impl<T: JsonRpcClient> ProtocolContracts<T> {
	pub fn new(
		is_native: bool,
		provider: Arc<Provider<T>>,
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
				H160::from_str(&socket_address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			),
			authority: AuthorityContract::new(
				H160::from_str(&authority_address).expect(INVALID_CONTRACT_ADDRESS),
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
				H160::from_str(&relayer_manager_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.bitcoin_socket = Some(BitcoinSocketContract::new(
				H160::from_str(&bitcoin_socket_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.socket_queue = Some(SocketQueueContract::new(
				H160::from_str(&socket_queue_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.registration_pool = Some(RegistrationPoolContract::new(
				H160::from_str(&registration_pool_address.expect(MISSING_CONTRACT_ADDRESS))
					.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.relay_executive = Some(RelayExecutiveContract::new(
				H160::from_str(&relay_executive_address.expect(MISSING_CONTRACT_ADDRESS))
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

/// The metadata of the EVM provider.
pub struct ProviderMetadata {
	pub name: String,
	/// Id of chain which this client interact with.
	pub id: ChainID,
	/// The total number of confirmations required for a block to be processed. (block
	/// confirmations + eth_getLogs batch size)
	pub block_confirmations: U64,
	/// The batch size used on `eth_getLogs()` requests.
	pub get_logs_batch_size: U64,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// Bridge direction when bridge event points this chain as destination.
	pub if_destination_chain: BridgeDirection,
	/// The flag whether the chain is BIFROST(native) or an external chain.
	pub is_native: bool,
}

impl ProviderMetadata {
	pub fn new(
		name: String,
		id: ChainID,
		block_confirmations: u64,
		call_interval: u64,
		get_logs_batch_size: u64,
		is_native: bool,
	) -> Self {
		Self {
			name,
			id,
			block_confirmations: U64::from(block_confirmations.saturating_add(get_logs_batch_size)),
			get_logs_batch_size: U64::from(get_logs_batch_size),
			call_interval,
			is_native,
			if_destination_chain: match is_native {
				true => BridgeDirection::Inbound,
				false => BridgeDirection::Outbound,
			},
		}
	}
}

/// The contract instances of the EVM provider.
pub struct ProviderContracts<T> {
	/// SocketContract
	pub socket: SocketContract<Provider<T>>,
	/// VaultContract
	pub vault: VaultContract<Provider<T>>,
	/// AuthorityContract
	pub authority: AuthorityContract<Provider<T>>,
	/// RelayerManagerContract (BIFROST only)
	pub relayer_manager: Option<RelayerManagerContract<Provider<T>>>,
	/// Chainlink usdc/usd aggregator
	pub chainlink_usdc_usd: Option<ChainlinkContract<Provider<T>>>,
	/// Chainlink usdt/usd aggregator
	pub chainlink_usdt_usd: Option<ChainlinkContract<Provider<T>>>,
	/// Chainlink dai/usd aggregator
	pub chainlink_dai_usd: Option<ChainlinkContract<Provider<T>>>,
}

impl<T: JsonRpcClient> ProviderContracts<T> {
	pub fn new(
		provider: Arc<Provider<T>>,
		socket_address: String,
		vault_address: String,
		authority_address: String,
		relayer_manager_address: Option<String>,
		chainlink_usdc_usd_address: Option<String>,
		chainlink_usdt_usd_address: Option<String>,
		chainlink_dai_usd_address: Option<String>,
	) -> Self {
		Self {
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
			relayer_manager: relayer_manager_address.map(|address| {
				RelayerManagerContract::new(
					H160::from_str(&address).expect(INVALID_CONTRACT_ADDRESS),
					provider.clone(),
				)
			}),
			chainlink_usdc_usd: chainlink_usdc_usd_address.map(|address| {
				ChainlinkContract::new(
					H160::from_str(&address).expect(INVALID_CONTRACT_ADDRESS),
					provider.clone(),
				)
			}),
			chainlink_usdt_usd: chainlink_usdt_usd_address.map(|address| {
				ChainlinkContract::new(
					H160::from_str(&address).expect(INVALID_CONTRACT_ADDRESS),
					provider.clone(),
				)
			}),
			chainlink_dai_usd: chainlink_dai_usd_address.map(|address| {
				ChainlinkContract::new(
					H160::from_str(&address).expect(INVALID_CONTRACT_ADDRESS),
					provider.clone(),
				)
			}),
		}
	}
}

#[derive(Clone, Debug)]
/// Coefficients to multiply the estimated gas amount.
pub enum GasCoefficient {
	Low,
	Mid,
	High,
}

impl GasCoefficient {
	pub fn into_f64(&self) -> f64 {
		match self {
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

impl From<&Uint8> for SocketEventStatus {
	fn from(value: &Uint8) -> Self {
		Self::from(u8::from(value.clone()))
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

impl EthClientConfiguration {
	pub fn new(
		name: String,
		id: u32,
		call_interval: u64,
		block_confirmations: U64,
		if_destination_chain: BridgeDirection,
	) -> Self {
		Self { name, id, call_interval, block_confirmations, if_destination_chain }
	}
}

#[derive(Clone, Debug, PartialEq)]
/// The state for bootstrapping
pub enum BootstrapState {
	/// phase 0. check if the node is in syncing
	NodeSyncing,
	/// phase 1. bootstrap for RoundUp event
	BootstrapRoundUp,
	/// phase 2. bootstrap for Socket event
	BootstrapSocket,
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
