use crate::eth::SocketEvents;

use super::EthClient;

use std::str::FromStr;

use cccp_primitives::eth::SOCKET_EVENT_SIG;
use ethers::{
	abi::RawLog,
	prelude::decode_logs,
	providers::JsonRpcClient,
	types::{TransactionReceipt, H256, U64},
};
use tokio::time::{sleep, Duration};
use tokio_stream::StreamExt;

/// The essential task that detects and parse CCCP-related events.
pub struct EventDetector<T> {
	/// The ethereum client for the connected chain.
	pub client: EthClient<T>,
}

impl<T: JsonRpcClient> EventDetector<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: EthClient<T>) -> Self {
		Self { client }
	}

	/// Starts the event detector. Reads every new mined block of the connected chain and starts to
	/// detect and store `Socket` transaction events.
	pub async fn run(&mut self) {
		// TODO: follow-up to the highest block
		loop {
			let latest_block = self.client.get_latest_block_number().await.unwrap();
			self.process_confirmed_block(latest_block).await;

			sleep(Duration::from_millis(self.client.config.call_interval)).await;
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
		println!("[{:?}] processed block: {:?}", self.client.config.name, block);
	}

	/// Decode and parse the socket event if the given transaction triggered an event.
	async fn process_confirmed_transaction(&mut self, receipt: TransactionReceipt) {
		if self.is_socket_contract(&receipt) {
			receipt.logs.iter().for_each(|log| {
				if Self::is_socket_event(log.topics[0]) {
					let raw_log: RawLog = log.clone().into();
					match decode_logs::<SocketEvents>(&[raw_log]) {
						Ok(decoded) => match &decoded[0] {
							SocketEvents::Socket(socket) =>
								self.client.push_event(socket.msg.clone()),
						},
						Err(error) => panic!(
							"[{:?}]-[{:?}] socket event decode error: {:?}",
							self.client.config.name, receipt.transaction_hash, error
						),
					}
				}
			})
		}
		println!("queue -> {:?}", self.client.event_queue);
	}

	/// Verifies whether the given transaction interacted with the socket contract.
	fn is_socket_contract(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			if to == self.client.config.socket_address {
				return true
			}
		}
		false
	}

	/// Verifies whether the given event topic matches the socket event signature.
	fn is_socket_event(topic: H256) -> bool {
		if topic == H256::from_str(SOCKET_EVENT_SIG).unwrap() {
			return true
		}
		false
	}
}
