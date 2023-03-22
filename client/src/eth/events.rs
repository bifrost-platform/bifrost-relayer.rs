use ethers::types::TransactionRequest;
use tokio::sync::mpsc::UnboundedSender;

pub struct EventChannel {
	// TODO: use transaction data as message type
	pub channel: UnboundedSender<TransactionRequest>,
	pub id: u32,
}

impl EventChannel {
	pub fn new(channel: UnboundedSender<TransactionRequest>, id: u32) -> Self {
		Self { channel, id }
	}
}
