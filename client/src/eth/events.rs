use std::fmt::Display;

use cccp_primitives::eth::SocketEventStatus;
use ethers::types::TransactionRequest;
use tokio::sync::mpsc::UnboundedSender;

/// The default retries of a single transaction request.
pub const DEFAULT_RETRIES: u8 = 3;

#[derive(Clone, Debug)]
pub struct RelayMetadata {
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

impl RelayMetadata {
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

impl Display for RelayMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Relay({}-{:?}-{:?}, {:?} -> {:?})",
			self.direction, self.status, self.sequence, self.src_chain_id, self.dst_chain_id
		)
	}
}

#[derive(Clone, Debug)]
pub enum EventMetadata {
	Relay(RelayMetadata),
	// TODO: add PriceFeed
}

impl Display for EventMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"{}",
			match self {
				EventMetadata::Relay(metadata) => metadata.to_string(),
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
}

impl EventSender {
	/// Instantiates a new `EventSender` instance.
	pub fn new(id: u32, sender: UnboundedSender<EventMessage>) -> Self {
		Self { id, sender }
	}
}
