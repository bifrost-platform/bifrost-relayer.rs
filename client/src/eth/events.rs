use std::{
	collections::BTreeMap,
	fmt::{Display, Formatter},
};

use ethers::types::{
	transaction::eip2718::TypedTransaction, Address, Bytes, Eip1559TransactionRequest,
	NameOrAddress, TransactionRequest, U256,
};
use tokio::sync::mpsc::{error::SendError, UnboundedSender};

use br_primitives::{
	eth::{ChainID, GasCoefficient, SocketEventStatus, SocketVariants},
	PriceResponse,
};

/// The default retries of a single json rpc request.
pub const DEFAULT_CALL_RETRIES: u8 = 3;

/// The default call retry interval in milliseconds.
pub const DEFAULT_CALL_RETRY_INTERVAL_MS: u64 = 3000;

/// The default retries of a single transaction request.
pub const DEFAULT_TX_RETRIES: u8 = 3;

/// The default transaction retry interval in milliseconds.
pub const DEFAULT_TX_RETRY_INTERVAL_MS: u64 = 3000;

/// The coefficient that will be multiplied on the max fee.
pub const MAX_FEE_COEFFICIENT: u64 = 2;

/// The coefficient that will be multipled on the max priority fee.
pub const MAX_PRIORITY_FEE_COEFFICIENT: u64 = 2;

#[derive(Clone, Debug)]
pub struct SocketRelayMetadata {
	/// The socket relay direction flag.
	pub is_inbound: bool,
	/// The socket event status.
	pub status: SocketEventStatus,
	/// The socket request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: ChainID,
	/// The destination chain ID.
	pub dst_chain_id: ChainID,
	/// The receiver address for this request.
	pub receiver: Address,
	/// The bridge amount.
	pub amount: U256,
	/// The source chain token index.
	pub token_idx0: [u8; 32],
	/// The destination chain token index.
	pub token_idx1: [u8; 32],
	/// The flag whether this relay is processed on bootstrap.
	pub is_bootstrap: bool,
	/// The variants this request contains.
	pub variants: SocketVariants,
}

impl SocketRelayMetadata {
	pub fn new(
		is_inbound: bool,
		status: SocketEventStatus,
		sequence: u128,
		src_chain_id: ChainID,
		dst_chain_id: ChainID,
		receiver: Address,
		is_bootstrap: bool,
	) -> Self {
		Self {
			is_inbound,
			status,
			sequence,
			src_chain_id,
			dst_chain_id,
			receiver,
			is_bootstrap,
			amount: Default::default(),
			token_idx0: Default::default(),
			token_idx1: Default::default(),
			variants: SocketVariants::default(),
		}
	}
}

impl Display for SocketRelayMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Relay({}-{:?}-{:?}, {:?} -> {:?}, {:?})",
			if self.is_inbound { "Inbound".to_string() } else { "Outbound".to_string() },
			self.status,
			self.sequence,
			self.src_chain_id,
			self.dst_chain_id,
			self.variants.version
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
	/// The round index to update.
	pub round: U256,
	/// The relayer addresses of the round.
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
	/// The round index to update.
	pub round: U256,
	/// The destination chain ID to update.
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
	/// The current round index.
	pub current_round_index: U256,
	/// The current session index.
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
pub struct RollbackMetadata {
	/// The socket relay direction flag.
	pub is_inbound: bool,
	/// The socket event status.
	pub status: SocketEventStatus,
	/// The socket request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: ChainID,
	/// The destination chain ID.
	pub dst_chain_id: ChainID,
}

impl RollbackMetadata {
	pub fn new(
		is_inbound: bool,
		status: SocketEventStatus,
		sequence: u128,
		src_chain_id: ChainID,
		dst_chain_id: ChainID,
	) -> Self {
		Self { is_inbound, status, sequence, src_chain_id, dst_chain_id }
	}
}

impl Display for RollbackMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Rollback({}-{:?}-{:?}, {:?} -> {:?})",
			if self.is_inbound { "Inbound".to_string() } else { "Outbound".to_string() },
			self.status,
			self.sequence,
			self.src_chain_id,
			self.dst_chain_id,
		)
	}
}

#[derive(Clone, Debug)]
pub enum EventMetadata {
	SocketRelay(SocketRelayMetadata),
	PriceFeed(PriceFeedMetadata),
	VSPPhase1(VSPPhase1Metadata),
	VSPPhase2(VSPPhase2Metadata),
	Heartbeat(HeartbeatMetadata),
	Flush(FlushMetadata),
	Rollback(RollbackMetadata),
}

impl Display for EventMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"{}",
			match self {
				EventMetadata::SocketRelay(metadata) => metadata.to_string(),
				EventMetadata::PriceFeed(metadata) => metadata.to_string(),
				EventMetadata::VSPPhase1(metadata) => metadata.to_string(),
				EventMetadata::VSPPhase2(metadata) => metadata.to_string(),
				EventMetadata::Heartbeat(metadata) => metadata.to_string(),
				EventMetadata::Flush(metadata) => metadata.to_string(),
				EventMetadata::Rollback(metadata) => metadata.to_string(),
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
	/// Get the `data` field of the transaction request.
	pub fn get_data(&self) -> &Bytes {
		match self {
			TxRequest::Legacy(tx_request) => tx_request.data.as_ref().unwrap(),
			TxRequest::Eip1559(tx_request) => tx_request.data.as_ref().unwrap(),
		}
	}

	/// Get the `to` field of the transaction request.
	pub fn get_to(&self) -> &NameOrAddress {
		match self {
			TxRequest::Legacy(tx_request) => tx_request.to.as_ref().unwrap(),
			TxRequest::Eip1559(tx_request) => tx_request.to.as_ref().unwrap(),
		}
	}

	/// Get the `from` field of the transaction request.
	pub fn get_from(&self) -> &Address {
		match self {
			TxRequest::Legacy(tx_request) => tx_request.from.as_ref().unwrap(),
			TxRequest::Eip1559(tx_request) => tx_request.from.as_ref().unwrap(),
		}
	}
	/// Get the `gas_price` field of the transaction request.
	pub fn get_gas_price(&self) -> Option<U256> {
		match self {
			TxRequest::Legacy(tx_request) => tx_request.gas_price,
			TxRequest::Eip1559(_) => None,
		}
	}

	/// Sets the `from` field in the transaction to the provided value.
	pub fn from(&mut self, address: Address) {
		match self {
			TxRequest::Legacy(tx_request) => {
				tx_request.from = Some(address);
			},
			TxRequest::Eip1559(tx_request) => {
				tx_request.from = Some(address);
			},
		}
	}

	/// Sets the `gas` field in the transaction to the provided.
	pub fn gas(&mut self, estimated_gas: U256) {
		match self {
			TxRequest::Legacy(tx_request) => {
				tx_request.gas = Some(estimated_gas);
			},
			TxRequest::Eip1559(tx_request) => {
				tx_request.gas = Some(estimated_gas);
			},
		}
	}

	/// Sets the `max_fee_per_gas` field in the transaction request.
	/// This method will only have effect when the type is EIP-1559.
	/// It will be ignored if the type is legacy.
	pub fn max_fee_per_gas(&mut self, max_fee_per_gas: U256) {
		match self {
			TxRequest::Legacy(_) => {},
			TxRequest::Eip1559(tx_request) => {
				tx_request.max_fee_per_gas = Some(max_fee_per_gas);
			},
		}
	}

	/// Sets the `max_priority_fee_per_gas` field in the transaction request.
	/// This method will only have effect when the type is EIP-1559.
	/// It will be ignored if the type is legacy.
	pub fn max_priority_fee_per_gas(&mut self, max_priority_fee_per_gas: U256) {
		match self {
			TxRequest::Legacy(_) => {},
			TxRequest::Eip1559(tx_request) => {
				tx_request.max_priority_fee_per_gas = Some(max_priority_fee_per_gas);
			},
		}
	}

	/// Sets the `gas_price` field in the transaction request.
	/// This method will only have effect when the type is legacy.
	/// It will be ignored if the type is EIP-1559.
	pub fn gas_price(&mut self, gas_price: U256) {
		match self {
			TxRequest::Legacy(tx_request) => {
				tx_request.gas_price = Some(gas_price);
			},
			TxRequest::Eip1559(_) => {},
		}
	}

	/// Sets the `nonce` field in the transaction request.
	pub fn nonce(&mut self, nonce: Option<U256>) {
		match self {
			TxRequest::Legacy(tx_request) => {
				tx_request.nonce = nonce;
			},
			TxRequest::Eip1559(tx_request) => {
				tx_request.nonce = nonce;
			},
		}
	}

	/// If self is Eip1559, returns it self.
	/// If self is Legacy, converts it self to Eip1559 and return it.
	pub fn to_eip1559(&self) -> Eip1559TransactionRequest {
		match self {
			TxRequest::Legacy(tx_request) => Eip1559TransactionRequest {
				from: tx_request.from,
				to: tx_request.to.clone(),
				value: tx_request.value,
				nonce: tx_request.nonce,
				data: tx_request.data.clone(),
				gas: tx_request.gas,
				..Default::default()
			},
			TxRequest::Eip1559(tx_request) => tx_request.clone(),
		}
	}

	/// If self is Eip1559, converts it self to Legacy and return it.
	/// If self is Legacy, returns it self.
	pub fn to_legacy(&self) -> TransactionRequest {
		match self {
			TxRequest::Legacy(tx_request) => tx_request.clone(),
			TxRequest::Eip1559(tx_request) => TransactionRequest {
				from: tx_request.from,
				to: tx_request.to.clone(),
				value: tx_request.value,
				nonce: tx_request.nonce,
				data: tx_request.data.clone(),
				gas: tx_request.gas,
				..Default::default()
			},
		}
	}

	/// Converts to `TypedTransaction`.
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
	/// The flag that represents whether the event is requested by a bootstrap process.
	/// If true, the event will be processed by a asynchronous task.
	pub is_bootstrap: bool,
}

impl EventMessage {
	/// Instantiates a new `EventMessage` instance.
	pub fn new(
		tx_request: TxRequest,
		metadata: EventMetadata,
		check_mempool: bool,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
		is_bootstrap: bool,
	) -> Self {
		Self {
			retries_remaining: DEFAULT_TX_RETRIES,
			retry_interval: DEFAULT_TX_RETRY_INTERVAL_MS,
			tx_request,
			metadata,
			check_mempool,
			give_random_delay,
			gas_coefficient,
			is_bootstrap,
		}
	}

	/// Builds a new `EventMessage` to use on transaction retry. This will reduce the remaining
	/// retry counter and increase the retry interval.
	pub fn build_retry_event(&mut self) {
		self.tx_request.nonce(None);
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

	/// Sends a new event message.
	pub fn send(&self, message: EventMessage) -> Result<(), SendError<EventMessage>> {
		self.sender.send(message)
	}
}
