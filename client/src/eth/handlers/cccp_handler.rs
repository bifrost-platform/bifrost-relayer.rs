use std::{str::FromStr, sync::Arc};

// TODO: Move event sig into handler structure (Initialize from config.yaml)
use cccp_primitives::eth::{BridgeDirection, Contract, SocketEventStatus, SOCKET_EVENT_SIG};
use ethers::{
	abi::RawLog,
	prelude::decode_logs,
	providers::{JsonRpcClient, Provider},
	types::{Bytes, TransactionReceipt, TransactionRequest, H160, H256, U256},
};
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventSender, Handler, PollSubmit, Signatures,
	SocketClient, SocketEvents, SocketExternal, SocketMessage, DEFAULT_RETRIES,
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
	pub target_contract: H160,
	/// The target `Socket` contract instance.
	pub target_socket: SocketExternal<Provider<T>>,
	/// The socket contracts supporting CCCP.
	pub socket_contracts: Vec<Contract>,
}

impl<T: JsonRpcClient> CCCPHandler<T> {
	/// Instantiates a new `CCCPHandler` instance.
	pub fn new(
		event_senders: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		client: Arc<EthClient<T>>,
		target_contract: H160,
		target_socket: H160,
		socket_contracts: Vec<Contract>,
	) -> Self {
		Self {
			event_senders,
			block_receiver,
			client: client.clone(),
			target_contract,
			target_socket: SocketExternal::new(target_socket, client.provider.clone()),
			socket_contracts,
		}
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

	fn request_send_transaction(&self, chain_id: u32, tx_request: TransactionRequest) {
		if let Some(event_sender) =
			self.event_senders.iter().find(|event_sender| event_sender.id == chain_id)
		{
			event_sender
				.sender
				.send(EventMessage::new(DEFAULT_RETRIES, tx_request))
				.unwrap();
		} else {
			panic!("[{:?}] invalid chain_id received : {:?}", self.client.config.name, chain_id)
		}
	}

	fn is_target_contract(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			if ethers::utils::to_checksum(&to, None) ==
				ethers::utils::to_checksum(&self.target_contract, None)
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

#[async_trait::async_trait]
impl<T: JsonRpcClient> SocketClient for CCCPHandler<T> {
	fn build_poll_call_data(&self, msg: SocketMessage, sigs: Signatures) -> Bytes {
		let poll_submit = PollSubmit { msg, sigs, option: U256::default() };
		self.target_socket.poll(poll_submit).calldata().unwrap()
	}

	async fn get_signatures(&self, msg: SocketMessage) -> Signatures {
		self.target_socket
			.get_signatures(msg.req_id, msg.status)
			.call()
			.await
			.unwrap_or_default()
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

		let to_socket = self
			.socket_contracts
			.iter()
			.find(|socket| socket.chain_id == relay_tx_chain_id)
			.unwrap()
			.address;
		// TODO: check how to set sigs. For now we just set as default.
		let mut tx_request = TransactionRequest::new();
		tx_request = tx_request
			.data(self.build_poll_call_data(msg, Signatures::default()))
			.to(to_socket);

		self.request_send_transaction(relay_tx_chain_id, tx_request);
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
