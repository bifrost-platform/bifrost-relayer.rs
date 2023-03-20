use crate::eth::SocketEvents;

use super::{EthClient, SocketMessage};

use std::{str::FromStr, sync::Arc};

use cccp_primitives::eth::{
	bfc_testnet, bsc_testnet, eth_testnet, polygon_testnet, SOCKET_EVENT_SIG,
};
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

pub struct EventChannels {
	pub bfc_channel: Sender<SocketMessage>,
	pub eth_channel: Sender<SocketMessage>,
	pub bsc_channel: Sender<SocketMessage>,
	pub polygon_channel: Sender<SocketMessage>,
}

impl EventChannels {
	pub fn new(
		bfc_channel: Sender<SocketMessage>,
		eth_channel: Sender<SocketMessage>,
		bsc_channel: Sender<SocketMessage>,
		polygon_channel: Sender<SocketMessage>,
	) -> Self {
		Self { bfc_channel, eth_channel, bsc_channel, polygon_channel }
	}
}

/// The essential task that detects and parse CCCP-related events.
pub struct EventDetector<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The channels sending socket messages.
	pub event_channels: Arc<EventChannels>,
}

impl<T: JsonRpcClient> EventDetector<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: Arc<EthClient<T>>, event_channels: Arc<EventChannels>) -> Self {
		Self { client, event_channels }
	}

	/// Starts the event detector. Reads every new mined block of the connected chain and starts to
	/// detect and store `Socket` transaction events.
	pub async fn run(&self) {
		// TODO: follow-up to the highest block
		loop {
			let latest_block = self.client.get_latest_block_number().await.unwrap();
			self.process_confirmed_block(latest_block).await;

			sleep(Duration::from_millis(self.client.config.call_interval)).await;
		}
	}

	/// Reads the contained transactions of the given confirmed block. This method will stream
	/// through the transaction array and retrieve its data.
	async fn process_confirmed_block(&self, block: U64) {
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
	async fn process_confirmed_transaction(&self, receipt: TransactionReceipt) {
		if self.is_socket_contract(&receipt) {
			let mut stream = tokio_stream::iter(receipt.logs);

			while let Some(log) = stream.next().await {
				if Self::is_socket_event(log.topics[0]) {
					let raw_log: RawLog = log.clone().into();
					match decode_logs::<SocketEvents>(&[raw_log]) {
						Ok(decoded) => match &decoded[0] {
							SocketEvents::Socket(socket) => {
								self.send_socket_message(socket.msg.clone()).await;
							},
						},
						Err(error) => panic!(
							"[{:?}]-[{:?}] socket event decode error: {:?}",
							self.client.config.name, receipt.transaction_hash, error
						),
					}
				}
			}
		}
	}

	/// Sends the `SocketMessage` to the `ins_code.chain` channel.
	async fn send_socket_message(&self, msg: SocketMessage) {
		let dst_chain_id = u32::from_be_bytes(msg.ins_code.chain);
		match dst_chain_id {
			bfc_testnet::BFC_CHAIN_ID => {
				self.event_channels.bfc_channel.send(msg).await.unwrap();
			},
			eth_testnet::ETH_CHAIN_ID => {
				self.event_channels.eth_channel.send(msg).await.unwrap();
			},
			bsc_testnet::BSC_CHAIN_ID => {
				self.event_channels.bsc_channel.send(msg).await.unwrap();
			},
			polygon_testnet::POLYGON_CHAIN_ID => {
				self.event_channels.polygon_channel.send(msg).await.unwrap();
			},
			_ => panic!(
				"[{:?}] invalid dst_chain_id received : {:?}",
				self.client.config.name, dst_chain_id
			),
		}
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
