use ethers::types::Eip1559TransactionRequest;
use tokio::sync::mpsc::UnboundedSender;

/// The default retries of a single transaction request.
pub const DEFAULT_RETRIES: u8 = 3;

#[derive(Clone, Debug)]
/// The message format passed through the event channel.
pub struct EventMessage {
	/// The remaining retries of the transaction request.
	pub retries_remaining: u8,
	/// The raw transaction request.
	pub tx_request: Eip1559TransactionRequest,
}

impl EventMessage {
	pub fn new(retries_remaining: u8, tx_request: Eip1559TransactionRequest) -> Self {
		Self { retries_remaining, tx_request }
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
	pub fn new(id: u32, sender: UnboundedSender<EventMessage>) -> Self {
		Self { id, sender }
	}
}
