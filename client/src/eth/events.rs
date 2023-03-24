use tokio::sync::mpsc::UnboundedSender;

use ethers::types::TransactionRequest;

/// The message sender connected to the event channel.
pub struct EventSender {
	/// The chain ID of the event channel.
	pub id: u32,
	/// The message sender.
	pub sender: UnboundedSender<TransactionRequest>,
}

impl EventSender {
	pub fn new(id: u32, sender: UnboundedSender<TransactionRequest>) -> Self {
		Self { id, sender }
	}
}
