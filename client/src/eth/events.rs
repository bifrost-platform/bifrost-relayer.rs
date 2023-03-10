use super::EthClient;

use tokio::time::{sleep, Duration};
use tokio_stream::StreamExt;
use web3::{
	types::{TransactionReceipt, U64},
	Transport,
};

/// The CCCP-related event data stored into the event queue.
pub struct EventData {}

/// The essential task that detects and parse CCCP-related events.
pub struct EventDetector<T: Transport> {
	/// The ethereum client for the connected chain.
	pub client: EthClient<T>,
}

impl<T: Transport> EventDetector<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: EthClient<T>) -> Self {
		Self { client }
	}

	/// Starts the event detector. Reads every new mined block of the connected chain and starts to
	/// detect and store CCCP-related transaction events.
	pub async fn run(&mut self) {
		// TODO: follow-up to the highest block
		loop {
			let latest_block = self.client.get_latest_block_number().await.unwrap();
			if let Some(confirmed_block) = self.client.try_push_block(latest_block) {
				self.process_confirmed_block(confirmed_block).await;
			}

			sleep(Duration::from_millis(self.client.config.call_interval)).await;
			println!("block number: {:?}", latest_block);
		}
	}

	/// Reads the contained transactions of the given confirmed block. This method will stream
	/// through the transaction array and retrieve its data.
	async fn process_confirmed_block(&mut self, block: U64) {
		if let Some(block) = self.client.get_block(block.into()).await.unwrap() {
			let mut stream = tokio_stream::iter(block.transactions);
			while let Some(tx) = stream.next().await {
				if let Some(receipt) = self.client.get_transaction_receipt(tx).await.unwrap() {
					self.process_confirmed_transaction(receipt).await;
				}
			}
		}
	}

	/// TODO: impl
	async fn process_confirmed_transaction(&mut self, tx: TransactionReceipt) {
		println!("receipt: {:?}", tx);
	}
}
