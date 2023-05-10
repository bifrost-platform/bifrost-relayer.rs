use cccp_primitives::{eth::SocketEventStatus, PriceResponse};

use ethers::types::{Address, TransactionRequest, U256};
use std::fmt::{Display, Formatter};
use tokio::sync::mpsc::UnboundedSender;

/// The default retries of a single json rpc request.
pub const DEFAULT_CALL_RETRIES: u8 = 3;

/// The default call retry interval in milliseconds.
pub const DEFAULT_CALL_RETRY_INTERVAL_MS: u64 = 3000;

/// The default retries of a single transaction request.
pub const DEFAULT_TX_RETRIES: u8 = 3;

/// The default transaction retry interval in milliseconds.
pub const DEFAULT_TX_RETRY_INTERVAL_MS: u64 = 3000;

/// The coefficient that will be multiplied to the retry interval on every new retry.
pub const RETRY_TX_COEFFICIENT: u64 = 2;

/// The coefficient that will be multiplied to the estimated gas.
pub const GAS_COEFFICIENT: f64 = 2.0;

#[derive(Clone, Debug)]
pub struct BridgeRelayMetadata {
	/// The bridge direction.
	pub direction: String,
	/// The bridge request status.
	pub status: SocketEventStatus,
	/// The bridge request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: u32,
	/// The destination chain ID.
	pub dst_chain_id: u32,
}

impl BridgeRelayMetadata {
	pub fn new(
		is_inbound: bool,
		status: SocketEventStatus,
		sequence: u128,
		src_chain_id: u32,
		dst_chain_id: u32,
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
	pub prices: Vec<PriceResponse>,
}

impl PriceFeedMetadata {
	pub fn new(prices: Vec<PriceResponse>) -> Self {
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
	pub dst_chain_id: u32,
}

impl VSPPhase2Metadata {
	pub fn new(round: U256, dst_chain_id: u32) -> Self {
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
pub enum EventMetadata {
	BridgeRelay(BridgeRelayMetadata),
	PriceFeed(PriceFeedMetadata),
	VSPPhase1(VSPPhase1Metadata),
	VSPPhase2(VSPPhase2Metadata),
	Heartbeat(HeartbeatMetadata),
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
			}
		)
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
	pub tx_request: TransactionRequest,
	/// Additional data of the transaction request.
	pub metadata: EventMetadata,
	/// Check mempool to prevent duplicate relay
	pub check_mempool: bool,
}

impl EventMessage {
	/// Instantiates a new `EventMessage` instance.
	pub fn new(
		tx_request: TransactionRequest,
		metadata: EventMetadata,
		check_mempool: bool,
	) -> Self {
		Self {
			retries_remaining: DEFAULT_TX_RETRIES,
			retry_interval: DEFAULT_TX_RETRY_INTERVAL_MS,
			tx_request,
			metadata,
			check_mempool,
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
	pub id: u32,
	/// The message sender.
	pub sender: UnboundedSender<EventMessage>,
	/// Is Bifrost network?
	pub is_native: bool,
}

impl EventSender {
	/// Instantiates a new `EventSender` instance.
	pub fn new(id: u32, sender: UnboundedSender<EventMessage>, is_native: bool) -> Self {
		Self { id, sender, is_native }
	}
}
