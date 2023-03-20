use crate::eth::SocketEvents;

use super::{EthClient, SocketMessage};

use std::{str::FromStr, sync::Arc};

use cccp_primitives::eth::{
	bfc_testnet, bsc_testnet, eth_testnet, polygon_testnet, SocketEventStatus, SOCKET_EVENT_SIG,
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

	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(&self, msg: SocketMessage) {
		let status = SocketEventStatus::from_u8(msg.status);
		if Self::is_sequence_ended(status) {
			return
		}

		let send_to_channel = |chain_id: u32, msg: SocketMessage| match chain_id {
			bfc_testnet::BFC_CHAIN_ID => self.event_channels.bfc_channel.send(msg),
			eth_testnet::ETH_CHAIN_ID => self.event_channels.eth_channel.send(msg),
			bsc_testnet::BSC_CHAIN_ID => self.event_channels.bsc_channel.send(msg),
			polygon_testnet::POLYGON_CHAIN_ID => self.event_channels.polygon_channel.send(msg),
			_ =>
				panic!("[{:?}] invalid chain_id received : {:?}", self.client.config.name, chain_id),
		};

		let relay_tx_chain_id = self.get_relay_tx_chain_id(
			status,
			u32::from_be_bytes(msg.req_id.chain),
			u32::from_be_bytes(msg.ins_code.chain),
			self.is_inbound_sequence(status),
		);
		send_to_channel(relay_tx_chain_id, msg).await.unwrap();
	}

	/// Get the chain ID of the relay transaction.
	fn get_relay_tx_chain_id(
		&self,
		status: SocketEventStatus,
		src_chain_id: u32,
		dst_chain_id: u32,
		is_inbound: bool,
	) -> u32 {
		match status {
			SocketEventStatus::Requested |
			SocketEventStatus::Executed |
			SocketEventStatus::Reverted =>
				if is_inbound {
					return dst_chain_id
				} else {
					return src_chain_id
				},
			SocketEventStatus::Accepted | SocketEventStatus::Rejected =>
				if is_inbound {
					return src_chain_id
				} else {
					return dst_chain_id
				},
			_ => panic!(
				"[{:?}] invalid socket event status received: {:?}",
				self.client.config.name, status
			),
		}
	}

	/// Verifies whether the socket event is a sequence of an inbound protocol.
	fn is_inbound_sequence(&self, status: SocketEventStatus) -> bool {
		if self.client.config.id == bfc_testnet::BFC_CHAIN_ID {
			// event detected on BIFROST
			match status {
				SocketEventStatus::Executed |
				SocketEventStatus::Reverted |
				SocketEventStatus::Accepted |
				SocketEventStatus::Rejected => return true,
				_ => return false,
			}
		} else {
			// event detected on any external chain
			match status {
				SocketEventStatus::Requested => return true,
				_ => return false,
			}
		}
	}

	/// Verifies whether the socket event status is `COMMITTED` or `ROLLBACKED`. If `true`,
	/// inbound|outbound sequence has been ended. No further actions required.
	fn is_sequence_ended(status: SocketEventStatus) -> bool {
		match status {
			SocketEventStatus::Committed | SocketEventStatus::Rollbacked => return true,
			_ => return false,
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
