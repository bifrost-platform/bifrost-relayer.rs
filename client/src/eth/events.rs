use super::{BlockChannel, EthClient};

use std::{str::FromStr, sync::Arc};

use cccp_primitives::eth::SOCKET_EVENT_SIG;
use ethers::{
	abi::RawLog,
	prelude::decode_logs,
	providers::JsonRpcClient,
	types::{TransactionReceipt, H256, U64},
};
use tokio::{
	sync::mpsc::Sender,
	time::{sleep, Duration},
};
use tokio_stream::StreamExt;

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

#[async_trait::async_trait]
pub trait EventHandler {
	fn new(event_channels: Arc<Vec<EventChannel>>, block_channel: Arc<BlockChannel>) -> Self;

	async fn run(&self);

	async fn process_confirmed_transaction(&self, receipt: TransactionReceipt);

	// TODO: add common methods
}
