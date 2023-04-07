use std::fmt::Display;

use cccp_primitives::{eth::SocketEventStatus, PriceResponse};
use ethers::types::{Address, TransactionRequest, U256};
use tokio::sync::mpsc::UnboundedSender;

/// The default retries of a single transaction request.
pub const DEFAULT_RETRIES: u8 = 3;

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
		direction: String,
		status: SocketEventStatus,
		sequence: u128,
		src_chain_id: u32,
		dst_chain_id: u32,
	) -> Self {
		Self { direction, status, sequence, src_chain_id, dst_chain_id }
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
	pub relayer_addresses: Vec<Address>,
}

impl VSPPhase1Metadata {
	pub fn new(relayer_addresses: Vec<Address>) -> Self {
		Self { relayer_addresses }
	}
}

impl Display for VSPPhase1Metadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let result = self
			.relayer_addresses
			.iter()
			.map(|address| address.to_string())
			.collect::<Vec<String>>();
		write!(f, "VSPPhase1({:?})", result,)
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
pub enum EventMetadata {
	BridgeRelay(BridgeRelayMetadata),
	PriceFeed(PriceFeedMetadata),
	VSPPhase1(VSPPhase1Metadata),
	VSPPhase2(VSPPhase2Metadata),
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
			}
		)
	}
}

#[derive(Clone, Debug)]
/// The message format passed through the event channel.
pub struct EventMessage {
	/// The remaining retries of the transaction request.
	pub retries_remaining: u8,
	/// The raw transaction request.
	pub tx_request: TransactionRequest,
	/// Additional data of the transaction request.
	pub metadata: EventMetadata,
}

impl EventMessage {
	/// Instantiates a new `EventMessage` instance.
	pub fn new(
		retries_remaining: u8,
		tx_request: TransactionRequest,
		metadata: EventMetadata,
	) -> Self {
		Self { retries_remaining, tx_request, metadata }
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
