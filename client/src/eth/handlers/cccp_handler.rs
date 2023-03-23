use std::{str::FromStr, sync::Arc};

use cccp_primitives::eth::{SocketEventStatus, SOCKET_EVENT_SIG}; /* TODO: Move event sig into handler structure
																  * (Initialize from config.yaml) */
use ethers::{
	abi::RawLog,
	prelude::{abigen, decode_logs},
	providers::JsonRpcClient,
	types::{TransactionReceipt, TransactionRequest, H160, H256},
};
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use crate::eth::{BlockMessage, EthClient, EventSender, Handler};

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
/// The `Socket` event from the `SocketExternal` contract.
pub struct Socket {
	pub msg: SocketMessage,
}

#[derive(Clone, ethers::contract::EthAbiType, Debug, PartialEq, Eq, Hash)]
/// The event enums originated from the `SocketExternal` contract.
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

/// The essential task that handles CCCP-related events.
pub struct CCCPHandler<T> {
	/// The event senders that sends messages to the event channel.
	pub event_senders: Vec<Arc<EventSender>>,
	/// The block receiver that consumes new blocks from the block channel.
	pub block_receiver: Receiver<BlockMessage>,
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The address of the `Socket` | `Vault` contract.
	pub contract: H160,
}

impl<T: JsonRpcClient> CCCPHandler<T> {
	/// Instantiates a new `CCCPHandler` instance.
	pub fn new(
		event_senders: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		client: Arc<EthClient<T>>,
		contract: H160,
	) -> Self {
		Self { event_senders, block_receiver, client, contract }
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for CCCPHandler<T> {
	async fn run(&mut self) {
		loop {
			let block_msg = self.block_receiver.recv().await.unwrap();
			let mut stream = tokio_stream::iter(block_msg.target_receipts);

			while let Some(receipt) = stream.next().await {
				self.process_confirmed_transaction(receipt).await;
			}
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
								self.send_socket_message(socket.msg.clone()).await;
							},
						},
						Err(error) => panic!("failed to decode socket event: {:?}", error),
					}
				}
			}
		}
	}

	async fn request_send_transaction(&self, dst_chain_id: u32, request: TransactionRequest) {
		// TODO: Make it works
		match self
			.event_senders
			.iter()
			.find(|sender| sender.id == dst_chain_id)
			.unwrap_or_else(|| {
				panic!(
					"[{:?}] invalid dst_chain_id received : {:?}",
					self.client.config.name, dst_chain_id
				)
			})
			.sender
			.send(request)
		{
			Ok(()) => println!("Request sent successfully"),
			Err(e) => println!("Failed to send request: {}", e),
		}
	}

	fn is_target_contract(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			if ethers::utils::to_checksum(&to, None) ==
				ethers::utils::to_checksum(&self.contract, None)
			{
				return true
			}
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

impl<T: JsonRpcClient> CCCPHandler<T> {
	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(&self, msg: SocketMessage) {
		let status = SocketEventStatus::from_u8(msg.status);
		if Self::is_sequence_ended(status) {
			// do nothing if protocol sequence ended
			return
		}

		let _send_to_channel = |chain_id: u32, msg: TransactionRequest| {
			if let Some(event_sender) =
				self.event_senders.iter().find(|event_sender| event_sender.id == chain_id)
			{
				event_sender.sender.send(msg)
			} else {
				panic!("[{:?}] invalid chain_id received : {:?}", self.client.config.name, chain_id)
			}
		};

		let src_chain_id = u32::from_be_bytes(msg.req_id.chain);
		let dst_chain_id = u32::from_be_bytes(msg.ins_code.chain);
		// let is_inbound = src_chain_id != bfc_testnet::BFC_CHAIN_ID;
		let is_inbound = true;

		let _relay_tx_chain_id = if is_inbound {
			self.get_inbound_relay_tx_chain_id(status, src_chain_id, dst_chain_id)
		} else {
			self.get_outbound_relay_tx_chain_id(status, src_chain_id, dst_chain_id)
		};
		// TODO: request_send_transaction
		// send_to_channel(relay_tx_chain_id, msg).await.unwrap();
	}

	/// Get the chain ID of the inbound sequence relay transaction.
	fn get_inbound_relay_tx_chain_id(
		&self,
		status: SocketEventStatus,
		src_chain_id: u32,
		dst_chain_id: u32,
	) -> u32 {
		match status {
			SocketEventStatus::Requested |
			SocketEventStatus::Executed |
			SocketEventStatus::Reverted => dst_chain_id,
			SocketEventStatus::Accepted | SocketEventStatus::Rejected => src_chain_id,
			_ => panic!(
				"[{:?}] invalid socket event status received: {:?}",
				self.client.config.name, status
			),
		}
	}

	/// Get the chain ID of the outbound sequence relay transaction.
	fn get_outbound_relay_tx_chain_id(
		&self,
		status: SocketEventStatus,
		src_chain_id: u32,
		dst_chain_id: u32,
	) -> u32 {
		match status {
			SocketEventStatus::Requested |
			SocketEventStatus::Executed |
			SocketEventStatus::Reverted => src_chain_id,
			SocketEventStatus::Accepted | SocketEventStatus::Rejected => dst_chain_id,
			_ => panic!(
				"[{:?}] invalid socket event status received: {:?}",
				self.client.config.name, status
			),
		}
	}

	/// Verifies whether the socket event status is `COMMITTED` or `ROLLBACKED`. If `true`,
	/// inbound|outbound sequence has been ended. No further actions required.
	fn is_sequence_ended(status: SocketEventStatus) -> bool {
		matches!(status, SocketEventStatus::Committed | SocketEventStatus::Rollbacked)
	}
}
