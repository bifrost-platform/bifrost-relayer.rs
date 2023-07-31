use std::{
	collections::BTreeMap,
	fmt::{Display, Formatter},
};

use ethers::types::{
	transaction::eip2718::TypedTransaction, Address, Eip1559TransactionRequest, TransactionRequest,
	U256,
};
use tokio::sync::mpsc::{error::SendError, UnboundedSender};

use br_primitives::{
	eth::{ChainID, GasCoefficient, SocketEventStatus},
	PriceResponse,
};

/// The default retries of a single json rpc request.
pub const DEFAULT_CALL_RETRIES: u8 = 3;

/// The default call retry interval in milliseconds.
pub const DEFAULT_CALL_RETRY_INTERVAL_MS: u64 = 3000;

/// The default retries of a single transaction request.
pub const DEFAULT_TX_RETRIES: u8 = 6;

/// The default transaction retry interval in milliseconds.
pub const DEFAULT_TX_RETRY_INTERVAL_MS: u64 = 3000;

/// The coefficient that will be multiplied to the retry interval on every new retry.
pub const RETRY_TX_COEFFICIENT: u64 = 2;

/// The coefficient that will be multiplied on the previously send transaction gas price.
pub const RETRY_GAS_PRICE_COEFFICIENT: f64 = 1.2;

/// The coefficient that will be multiplied on the max fee.
pub const MAX_FEE_COEFFICIENT: u64 = 2;

/// The coefficient that will be multipled on the max priority fee.
pub const MAX_PRIORITY_FEE_COEFFICIENT: u64 = 2;

#[derive(Clone, Debug)]
pub struct BridgeRelayMetadata {
	/// The bridge direction.
	pub direction: String,
	/// The bridge request status.
	pub status: SocketEventStatus,
	/// The bridge request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: ChainID,
	/// The destination chain ID.
	pub dst_chain_id: ChainID,
}

impl BridgeRelayMetadata {
	pub fn new(
		is_inbound: bool,
		status: SocketEventStatus,
		sequence: u128,
		src_chain_id: ChainID,
		dst_chain_id: ChainID,
	) -> Self {
		Self {
			direction: if is_inbound { "Inbound".to_string() } else { "Outbound".to_string() },
			status,
			sequence,
			src_chain_id,
			dst_chain_id,
		}
	}
}

impl Display for BridgeRelayMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Relay({}-{:?}-{:?}, {:?} -> {:?})",
			self.direction, self.status, self.sequence, self.src_chain_id, self.dst_chain_id
		)
	}
}

#[derive(Clone, Debug)]
pub struct PriceFeedMetadata {
	/// The fetched price responses mapped to token symbol.
	pub prices: BTreeMap<String, PriceResponse>,
}

impl PriceFeedMetadata {
	pub fn new(prices: BTreeMap<String, PriceResponse>) -> Self {
		Self { prices }
	}
}

impl Display for PriceFeedMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "PriceFeed({:?})", self.prices)
	}
}

#[derive(Clone, Debug)]
pub struct VSPPhase1Metadata {
	pub round: U256,
	pub relayer_addresses: Vec<Address>,
}

impl VSPPhase1Metadata {
	pub fn new(round: U256, relayer_addresses: Vec<Address>) -> Self {
		Self { round, relayer_addresses }
	}
}

impl Display for VSPPhase1Metadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let relayers = self
			.relayer_addresses
			.iter()
			.map(|address| address.to_string())
			.collect::<Vec<String>>();
		write!(f, "VSPPhase1({:?}, {:?})", self.round, relayers)
	}
}

#[derive(Clone, Debug)]
pub struct VSPPhase2Metadata {
	pub round: U256,
	pub dst_chain_id: ChainID,
}

impl VSPPhase2Metadata {
	pub fn new(round: U256, dst_chain_id: ChainID) -> Self {
		Self { round, dst_chain_id }
	}
}

impl Display for VSPPhase2Metadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "VSPPhase2({:?}, On chain:{:?})", self.round, self.dst_chain_id)
	}
}

#[derive(Clone, Debug)]
pub struct HeartbeatMetadata {
	pub current_round_index: U256,
	pub current_session_index: U256,
}

impl HeartbeatMetadata {
	pub fn new(current_round_index: U256, current_session_index: U256) -> Self {
		Self { current_round_index, current_session_index }
	}
}

impl Display for HeartbeatMetadata {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Heartbeat(Round: {:?}, Session: {:?})",
			self.current_round_index, self.current_session_index
		)
	}
}

#[derive(Clone, Debug)]
pub struct FlushMetadata {}

impl FlushMetadata {
	pub fn new() -> Self {
		Self {}
	}
}

impl Default for FlushMetadata {
	fn default() -> Self {
		Self::new()
	}
}

impl Display for FlushMetadata {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "Flush mempool")
	}
}

#[derive(Clone, Debug)]
pub enum EventMetadata {
	BridgeRelay(BridgeRelayMetadata),
	PriceFeed(PriceFeedMetadata),
	VSPPhase1(VSPPhase1Metadata),
	VSPPhase2(VSPPhase2Metadata),
	Heartbeat(HeartbeatMetadata),
	Flush(FlushMetadata),
}

impl Display for EventMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"{}",
			match self {
				EventMetadata::BridgeRelay(metadata) => metadata.to_string(),
				EventMetadata::PriceFeed(metadata) => metadata.to_string(),
				EventMetadata::VSPPhase1(metadata) => metadata.to_string(),
				EventMetadata::VSPPhase2(metadata) => metadata.to_string(),
				EventMetadata::Heartbeat(metadata) => metadata.to_string(),
				EventMetadata::Flush(metadata) => metadata.to_string(),
			}
		)
	}
}

/// Wrapper for TransactionRequest|Eip1559TransactionRequest to support both fee payment in one
/// relayer
#[derive(Clone, Debug)]
pub enum TxRequest {
	Legacy(TransactionRequest),
	Eip1559(Eip1559TransactionRequest),
}

impl TxRequest {
	/// Sets the `from` field in the transaction to the provided value
	pub fn from(&self, address: Address) -> Self {
		match self {
			TxRequest::Legacy(tx_request) => TxRequest::Legacy(tx_request.clone().from(address)),
			TxRequest::Eip1559(tx_request) => TxRequest::Eip1559(tx_request.clone().from(address)),
		}
	}

	/// Sets the `gas` field in the transaction to the provided
	pub fn gas(&self, estimated_gas: U256) -> Self {
		match self {
			TxRequest::Legacy(tx_request) =>
				TxRequest::Legacy(tx_request.clone().gas(estimated_gas)),
			TxRequest::Eip1559(tx_request) =>
				TxRequest::Eip1559(tx_request.clone().gas(estimated_gas)),
		}
	}

	/// If self is Eip1559, returns it self.
	/// If self is Legacy, converts it self to Eip1559 and return it.
	pub fn to_eip1559(&self) -> Eip1559TransactionRequest {
		match self {
			TxRequest::Legacy(tx_request) => {
				let mut ret = Eip1559TransactionRequest::default();
				ret.from = tx_request.from;
				ret.to = tx_request.to.clone();
				ret.value = tx_request.value;
				ret.nonce = tx_request.nonce;
				ret.data = tx_request.data.clone();
				ret.gas = tx_request.gas;
				ret
			},
			TxRequest::Eip1559(tx_request) => tx_request.clone(),
		}
	}

	/// If self is Eip1559, converts it self to Legacy and return it.
	/// If self is Legacy, returns it self.
	pub fn to_legacy(&self) -> TransactionRequest {
		match self {
			TxRequest::Legacy(tx_request) => tx_request.clone(),
			TxRequest::Eip1559(tx_request) => {
				let mut ret = TransactionRequest::default();
				ret.from = tx_request.from;
				ret.to = tx_request.to.clone();
				ret.value = tx_request.value;
				ret.nonce = tx_request.nonce;
				ret.data = tx_request.data.clone();
				ret.gas = tx_request.gas;
				ret
			},
		}
	}

	/// Converts to `TypedTransaction`
	pub fn to_typed(&self) -> TypedTransaction {
		match self {
			TxRequest::Legacy(tx_request) => TypedTransaction::Legacy(tx_request.clone()),
			TxRequest::Eip1559(tx_request) => TypedTransaction::Eip1559(tx_request.clone()),
		}
	}
}

#[derive(Clone, Debug)]
/// The message format passed through the event channel.
pub struct EventMessage {
	/// The remaining retries of the transaction request.
	pub retries_remaining: u8,
	/// The retry interval in milliseconds.
	pub retry_interval: u64,
	/// The raw transaction request.
	pub tx_request: TxRequest,
	/// Additional data of the transaction request.
	pub metadata: EventMetadata,
	/// Check mempool to prevent duplicate relay.
	pub check_mempool: bool,
	/// The flag that represents whether the event is processed to an external chain.
	pub give_random_delay: bool,
	/// The gas coefficient that will be multiplied to the estimated gas amount.
	pub gas_coefficient: GasCoefficient,
}

impl EventMessage {
	/// Instantiates a new `EventMessage` instance.
	pub fn new(
		tx_request: TxRequest,
		metadata: EventMetadata,
		check_mempool: bool,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) -> Self {
		Self {
			retries_remaining: DEFAULT_TX_RETRIES,
			retry_interval: DEFAULT_TX_RETRY_INTERVAL_MS,
			tx_request,
			metadata,
			check_mempool,
			give_random_delay,
			gas_coefficient,
		}
	}

	/// Builds a new `EventMessage` to use on transaction retry. This will reduce the remaining
	/// retry counter and increase the retry interval.
	pub fn build_retry_event(&mut self) {
		// do not multiply the coefficient on the first retry
		if self.retries_remaining != DEFAULT_TX_RETRIES {
			self.retry_interval = self.retry_interval.saturating_mul(RETRY_TX_COEFFICIENT);
		}
		self.retries_remaining = self.retries_remaining.saturating_sub(1);
	}
}

/// The message sender connected to the event channel.
pub struct EventSender {
	/// The chain ID of the event channel.
	pub id: ChainID,
	/// The message sender.
	pub sender: UnboundedSender<EventMessage>,
	/// Is Bifrost network?
	pub is_native: bool,
}

impl EventSender {
	/// Instantiates a new `EventSender` instance.
	pub fn new(id: ChainID, sender: UnboundedSender<EventMessage>, is_native: bool) -> Self {
		Self { id, sender, is_native }
	}

	pub fn send(&self, message: EventMessage) -> Result<(), SendError<EventMessage>> {
		self.sender.send(message)
	}
}
