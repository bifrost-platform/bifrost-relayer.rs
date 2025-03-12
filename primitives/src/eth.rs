use std::{str::FromStr, sync::Arc};

use alloy::{
	network::AnyNetwork,
	primitives::{Address, ChainId},
	providers::{
		Provider, WalletProvider,
		fillers::{FillProvider, TxFiller},
	},
	rpc::types::TransactionRequest,
};
use url::Url;

use crate::{
	cli::EVMProvider,
	constants::{
		cli::DEFAULT_GET_LOGS_BATCH_SIZE,
		errors::{INVALID_CONTRACT_ADDRESS, MISSING_CONTRACT_ADDRESS},
	},
	contracts::{
		authority::{AuthorityContract, AuthorityInstance},
		bitcoin_socket::{BitcoinSocketContract, BitcoinSocketInstance},
		chainlink_aggregator::{ChainlinkContract, ChainlinkInstance},
		registration_pool::{RegistrationPoolContract, RegistrationPoolInstance},
		relay_executive::{RelayExecutiveContract, RelayExecutiveInstance},
		relayer_manager::{RelayerManagerContract, RelayerManagerInstance},
		socket::{SocketContract, SocketInstance},
		socket_queue::{SocketQueueContract, SocketQueueInstance},
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
	/// The flag whether EIP-1559 is enabled.
	pub eip1559: bool,
	/// Relay direction when CCCP event points this chain as destination.
	pub if_destination_chain: RelayDirection,
	/// The flag whether the chain is Bifrost(native) or an external chain.
	pub is_native: bool,
	/// The flag whether the chain is relay target.
	pub is_relay_target: bool,
}

impl ProviderMetadata {
	pub fn new(
		evm_provider: EVMProvider,
		url: Url,
		bitcoin_chain_id: Option<ChainId>,
		is_native: bool,
	) -> Self {
		let get_logs_batch_size =
			evm_provider.get_logs_batch_size.unwrap_or(DEFAULT_GET_LOGS_BATCH_SIZE);
		Self {
			name: evm_provider.name,
			url,
			id: evm_provider.id,
			bitcoin_chain_id,
			block_confirmations: evm_provider
				.block_confirmations
				.saturating_add(get_logs_batch_size),
			get_logs_batch_size,
			call_interval: evm_provider.call_interval,
			eip1559: evm_provider.eip1559.unwrap_or(false),
			is_native,
			if_destination_chain: match is_native {
				true => RelayDirection::Inbound,
				false => RelayDirection::Outbound,
			},
			is_relay_target: evm_provider.is_relay_target,
		}
	}
}

#[derive(Clone)]
pub struct AggregatorContracts<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// Chainlink usdc/usd aggregator
	pub chainlink_usdc_usd: Option<ChainlinkInstance<F, P>>,
	/// Chainlink usdt/usd aggregator
	pub chainlink_usdt_usd: Option<ChainlinkInstance<F, P>>,
	/// Chainlink dai/usd aggregator
	pub chainlink_dai_usd: Option<ChainlinkInstance<F, P>>,
	/// Chainlink btc/usd aggregator
	pub chainlink_btc_usd: Option<ChainlinkInstance<F, P>>,
	/// Chainlink wbtc/usd aggregator
	pub chainlink_wbtc_usd: Option<ChainlinkInstance<F, P>>,
	/// Chainlink cbbtc/usd aggregator
	pub chainlink_cbbtc_usd: Option<ChainlinkInstance<F, P>>,
}

impl<F, P> AggregatorContracts<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	pub fn new(
		provider: Arc<FillProvider<F, P, AnyNetwork>>,
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

impl<F, P> Default for AggregatorContracts<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
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
pub struct ProtocolContracts<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// SocketContract
	pub socket: SocketInstance<F, P>,
	/// AuthorityContract
	pub authority: AuthorityInstance<F, P>,
	/// RelayerManagerContract (Bifrost only)
	pub relayer_manager: Option<RelayerManagerInstance<F, P>>,
	/// BitcoinSocketContract (Bifrost only)
	pub bitcoin_socket: Option<BitcoinSocketInstance<F, P>>,
	/// SocketQueueContract (Bifrost only)
	pub socket_queue: Option<SocketQueueInstance<F, P>>,
	/// RegistrationPoolContract (Bifrost only)
	pub registration_pool: Option<RegistrationPoolInstance<F, P>>,
	/// RelayExecutiveContract (Bifrost only)
	pub relay_executive: Option<RelayExecutiveInstance<F, P>>,
}

impl<F, P> ProtocolContracts<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	pub fn new(
		is_native: bool,
		provider: Arc<FillProvider<F, P, AnyNetwork>>,
		evm_provider: EVMProvider,
	) -> Self {
		let mut contracts = Self {
			socket: SocketContract::new(
				Address::from_str(&evm_provider.socket_address).expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			),
			authority: AuthorityContract::new(
				Address::from_str(&evm_provider.authority_address).expect(INVALID_CONTRACT_ADDRESS),
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
				Address::from_str(
					&evm_provider.relayer_manager_address.expect(MISSING_CONTRACT_ADDRESS),
				)
				.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.bitcoin_socket = Some(BitcoinSocketContract::new(
				Address::from_str(
					&evm_provider.bitcoin_socket_address.expect(MISSING_CONTRACT_ADDRESS),
				)
				.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.socket_queue = Some(SocketQueueContract::new(
				Address::from_str(
					&evm_provider.socket_queue_address.expect(MISSING_CONTRACT_ADDRESS),
				)
				.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.registration_pool = Some(RegistrationPoolContract::new(
				Address::from_str(
					&evm_provider.registration_pool_address.expect(MISSING_CONTRACT_ADDRESS),
				)
				.expect(INVALID_CONTRACT_ADDRESS),
				provider.clone(),
			));
			contracts.relay_executive = Some(RelayExecutiveContract::new(
				Address::from_str(
					&evm_provider.relay_executive_address.expect(MISSING_CONTRACT_ADDRESS),
				)
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

impl From<bool> for GasCoefficient {
	fn from(is_native: bool) -> Self {
		if is_native { GasCoefficient::Mid } else { GasCoefficient::Low }
	}
}

impl From<GasCoefficient> for f64 {
	fn from(value: GasCoefficient) -> Self {
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

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// The state for bootstrapping
pub enum BootstrapState {
	/// phase 0-1. check if the node is in syncing
	NodeSyncing,
	/// phase 0-2. check if stalled transactions are flushed
	FlushingStalledTransactions,
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
