use std::{str::FromStr, sync::Arc};

use cccp_primitives::eth::SOCKET_EVENT_SIG;
use ethers::{
	abi::RawLog,
	prelude::{abigen, decode_logs},
	providers::JsonRpcClient,
	types::{TransactionReceipt, H256},
};
use tokio_stream::StreamExt;

use crate::eth::{BlockChannel, EthClient, EventChannel, EventHandler};

abigen!(
	SocketExternal,
	"../abi/abi.socket.external.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

#[derive(
	Clone,
	ethers::contract::EthEvent,
	ethers::contract::EthDisplay,
	Default,
	Debug,
	PartialEq,
	Eq,
	Hash,
)]
#[ethevent(
	name = "Socket",
	abi = "Socket(((bytes4,uint64,uint128),uint8,(bytes4,bytes16),(bytes32,bytes32,address,address,uint256,bytes)))"
)]
pub struct Socket {
	pub msg: SocketMessage,
}

#[derive(Clone, ethers::contract::EthAbiType, Debug, PartialEq, Eq, Hash)]
pub enum SocketEvents {
	Socket(Socket),
}

impl ethers::contract::EthLogDecode for SocketEvents {
	fn decode_log(log: &RawLog) -> Result<Self, ethers::abi::Error>
	where
		Self: Sized,
	{
		if let Ok(decoded) = Socket::decode_log(log) {
			return Ok(SocketEvents::Socket(decoded))
		}
		Err(ethers::abi::Error::InvalidData)
	}
}

/// The essential task that detects and parse CCCP-related events.
pub struct SocketHandler {
	/// The channels sending socket messages.
	pub event_channels: Arc<Vec<EventChannel>>,
	pub block_channel: Arc<BlockChannel>,
}

#[async_trait::async_trait]
impl EventHandler for SocketHandler {
	/// Instantiates a new `EventDetector` instance.
	fn new(event_channels: Arc<Vec<EventChannel>>, block_channel: Arc<BlockChannel>) -> Self {
		Self { event_channels, block_channel }
	}

	async fn run(&self) {
		loop {
			// TODO: read block data
			println!("test");
		}
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
						Err(error) => panic!("panic"),
					}
				}
			}
		}
	}
}

impl SocketHandler {
	/// Sends the `SocketMessage` to the `ins_code.chain` channel.
	async fn send_socket_message(&self, msg: SocketMessage) {
		let _dst_chain_id = u32::from_be_bytes(msg.ins_code.chain);

		// self.event_channels
		// 	.iter()
		// 	.find(|channel| channel.id == dst_chain_id)
		// 	.unwrap_or_else(|| {
		// 		panic!(
		// 			"[{:?}] invalid dst_chain_id received : {:?}",
		// 			self.client.config.name, dst_chain_id
		// 		)
		// 	})
		// 	.channel
		// 	.send(msg)
		// 	.await
		// 	.unwrap();
	}

	/// Verifies whether the given transaction interacted with the socket contract.
	fn is_socket_contract(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			// if to == self.client.config.socket_address {
			// 	return true
			// }
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
