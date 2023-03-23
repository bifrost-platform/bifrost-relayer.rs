use std::{str::FromStr, sync::Arc};

// TODO: Move event sig into handler structure (Initialize from config.yaml)
use cccp_primitives::eth::{BridgeDirection, SocketEventStatus, SOCKET_EVENT_SIG};
use ethers::{
	abi::RawLog,
	prelude::decode_logs,
	providers::JsonRpcClient,
	types::{Eip1559TransactionRequest, TransactionReceipt, H160, H256},
};
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use crate::eth::{
	BlockMessage, EthClient, EventSender, Handler, Signatures, SocketClient, SocketEvents,
	SocketMessage,
};

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

			println!(
				"[{:?}]-[cccp-handler] received block: {:?}",
				self.client.get_chain_name(),
				block_msg.raw_block.number
			);

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
								println!(
									"[{:?}]-[cccp-handler] detected socket event: {:?}-{:?}-{:?}",
									self.client.get_chain_name(),
									socket.msg.status,
									u32::from_be_bytes(socket.msg.req_id.chain),
									u32::from_be_bytes(socket.msg.ins_code.chain),
								);
								self.send_socket_message(socket.msg.clone()).await;
							},
						},
						Err(error) => panic!("failed to decode socket event: {:?}", error),
					}
				}
			}
		}
	}

	fn request_send_transaction(&self, chain_id: u32, raw_tx: Eip1559TransactionRequest) {
		if let Some(event_sender) =
			self.event_senders.iter().find(|event_sender| event_sender.id == chain_id)
		{
			event_sender.sender.send(raw_tx).unwrap();
		} else {
			panic!("[{:?}] invalid chain_id received : {:?}", self.client.config.name, chain_id)
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

		let src_chain_id = u32::from_be_bytes(msg.req_id.chain);
		let dst_chain_id = u32::from_be_bytes(msg.ins_code.chain);
		let is_inbound = self.is_inbound_sequence(dst_chain_id);

		let relay_tx_chain_id = if is_inbound {
			self.get_inbound_relay_tx_chain_id(status, src_chain_id, dst_chain_id)
		} else {
			self.get_outbound_relay_tx_chain_id(status, src_chain_id, dst_chain_id)
		};

		let mut raw_tx = Eip1559TransactionRequest::new();
		// TODO: check how to set sigs. For now we just set as default.
		raw_tx = raw_tx.data(self.client.build_poll_call_data(msg, Signatures::default()));

		self.request_send_transaction(relay_tx_chain_id, raw_tx);
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

	/// Verifies whether the socket event is an inbound sequence.
	fn is_inbound_sequence(&self, dst_chain_id: u32) -> bool {
		match (self.client.get_chain_id() == dst_chain_id, self.client.config.if_destination_chain)
		{
			(true, BridgeDirection::Inbound) | (false, BridgeDirection::Outbound) => true,
			_ => false,
		}
	}
}
