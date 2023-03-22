use std::{str::FromStr, sync::Arc};

use cccp_primitives::eth::SOCKET_EVENT_SIG; /* TODO: Move event sig into handler structure
											 * (Initialize from config.yaml) */
use ethers::{
	abi::RawLog,
	prelude::{abigen, decode_logs},
	providers::JsonRpcClient,
	types::{TransactionReceipt, TransactionRequest, H256},
};
use tokio_stream::StreamExt;

use crate::eth::{BlockReceiver, EthClient, EventChannel, Handler};

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
pub struct CCCPHandler<T> {
	/// The channels sending socket messages.
	pub event_channels: Arc<Vec<EventChannel>>,
	/// The channels receiving new block with transactions.
	pub block_channel: Arc<BlockReceiver>,
	/// EthClient to interact with blockchain.
	pub client: Arc<EthClient<T>>,
	/// The address of socket | vault contract.
	pub contract: String,
}

impl<T: JsonRpcClient> CCCPHandler<T> {
	/// The constructor of SocketHandler
	pub fn new(
		event_channels: Arc<Vec<EventChannel>>,
		block_channel: Arc<BlockReceiver>,
		client: Arc<EthClient<T>>,
		contract: String,
	) -> Self {
		Self { event_channels, block_channel, client, contract }
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for CCCPHandler<T> {
	async fn run(&self) {
		loop {
			// TODO: read block data
			println!("test");
		}
	}

	async fn process_confirmed_transaction(&self, receipt: TransactionReceipt) {
		if self.is_target_contract(&receipt) {
			let mut stream = tokio_stream::iter(receipt.logs);

			while let Some(log) = stream.next().await {
				if Self::is_target_event(log.topics[0]) {
					let raw_log: RawLog = log.clone().into();
					match decode_logs::<SocketEvents>(&[raw_log]) {
						Ok(decoded) => match &decoded[0] {
							SocketEvents::Socket(socket) => {
								// TODO: Edit under to use self.request_send_transaction()
								// self.send_socket_message(socket.msg.clone()).await;
							},
						},
						Err(error) => panic!("panic"), // TODO: Print fail reason on panic
					}
				}
			}
		}
	}

	async fn request_send_transaction(&self, dst_chain_id: u32, request: TransactionRequest) {
		// TODO: Make it works
		match self
			.event_channels
			.iter()
			.find(|channel| channel.id == dst_chain_id)
			.unwrap_or_else(|| {
				panic!(
					"[{:?}] invalid dst_chain_id received : {:?}",
					self.client.config.name, dst_chain_id
				)
			})
			.channel
			.send(request)
		{
			Ok(()) => println!("Request sent successfully"),
			Err(e) => println!("Failed to send request: {}", e),
		}
	}

	fn is_target_contract(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			// if to == self.client.config.socket_address {
			// 	return true
			// }
		}
		false
	}

	fn is_target_event(topic: H256) -> bool {
		if topic == H256::from_str(SOCKET_EVENT_SIG).unwrap() {
			return true
		}
		false
	}
}
