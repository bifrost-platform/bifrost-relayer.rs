use ethers::types::TransactionRequest;
use tokio::sync::mpsc::Sender;

pub struct EventChannel {
	// TODO: use transaction data as message type
	pub channel: Sender<TransactionRequest>,
	pub id: u32,
}

impl EventChannel {
	pub fn new(channel: Sender<TransactionRequest>, id: u32) -> Self {
		Self { channel, id }
	}
}
