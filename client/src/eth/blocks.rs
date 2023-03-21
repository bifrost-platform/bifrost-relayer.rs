use std::sync::Arc;

use ethers::{
	providers::JsonRpcClient,
	types::{Block, H256},
};
use tokio::{
	sync::watch::{self, Receiver, Sender},
	time::{sleep, Duration},
};

use super::EthClient;

pub type BlockChannel = Receiver<Option<Block<H256>>>;

/// The essential task that detects and parse CCCP-related events.
pub struct BlockManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The channel sending block messages.
	pub latest_block: Sender<Option<Block<H256>>>,
}

impl<T: JsonRpcClient> BlockManager<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, Arc<BlockChannel>) {
		let (latest_block, receiver) = watch::channel(None);
		(Self { client, latest_block }, Arc::new(receiver))
	}

	/// Starts the event detector. Reads every new mined block of the connected chain and starts to
	/// detect and store `Socket` transaction events.
	pub async fn run(&self) {
		// TODO: follow-up to the highest block
		loop {
			let _latest_block = self.client.get_latest_block_number().await.unwrap();
			// TODO: read block and send

			sleep(Duration::from_millis(self.client.config.call_interval)).await;
		}
	}
}
