use super::EthClient;

use ethers::types::U64;
use tokio::sync::mpsc::Sender;

pub struct EventChannel {
	// TODO: use transaction data as message type
	pub channel: Sender<u32>,
	pub id: u32,
}

impl EventChannel {
	pub fn new(channel: Sender<u32>, id: u32) -> Self {
		Self { channel, id }
	}
}
