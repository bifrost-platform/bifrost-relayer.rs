use std::{str::FromStr, sync::Arc};

// TODO: Move event sig into handler structure (Initialize from config.yaml)
use cccp_primitives::eth::{BridgeDirection, Contract, SocketEventStatus, SOCKET_EVENT_SIG};
use ethers::{
	abi::{encode, RawLog, Token},
	prelude::decode_logs,
	providers::{JsonRpcClient, Provider},
	signers::Signer,
	types::{Bytes, Signature, TransactionReceipt, TransactionRequest, H160, H256, U256},
};
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler, PollSubmit,
	RelayMetadata, Signatures, SocketClient, SocketEvents, SocketExternal, SocketMessage,
	DEFAULT_RETRIES,
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

			log::info!(
				target: &self.client.get_chain_name(),
				"-[cccp-handler       ] ✨ Imported #{:?} ({}) with target transactions({:?})",
				block_msg.raw_block.number.unwrap(),
				block_msg.raw_block.hash.unwrap(),
				block_msg.target_receipts.len(),
			);

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
								let src_chain_id = u32::from_be_bytes(socket.msg.req_id.chain);
								let dst_chain_id = u32::from_be_bytes(socket.msg.ins_code.chain);
								let is_inbound = self.is_inbound_sequence(dst_chain_id);

								let metadata = RelayMetadata::new(
									if is_inbound {
										"Inbound".to_string()
									} else {
										"Outbound".to_string()
									},
									SocketEventStatus::from_u8(socket.msg.status),
									socket.msg.req_id.sequence,
									src_chain_id,
									dst_chain_id,
								);

								log::info!(
									target: &self.client.get_chain_name(),
									"-[cccp-handler       ] 🔖 Detected socket event: {}, {:?}-{:?}",
									metadata,
									receipt.block_number.unwrap(),
									receipt.transaction_hash,
								);

								self.send_socket_message(socket.msg.clone(), metadata, is_inbound)
									.await;
							},
						},
						Err(error) => panic!(
								"{}]-[cccp-handler       ] Unknown error while decoding socket event: {:?}",
								self.client.get_chain_name(),
								error
							),
					}
				}
			}
		}
	}

	fn request_send_transaction(
		&self,
		chain_id: u32,
		tx_request: TransactionRequest,
		metadata: RelayMetadata,
	) {
		if let Some(event_sender) =
			self.event_senders.iter().find(|event_sender| event_sender.id == chain_id)
		{
			event_sender
				.sender
				.send(EventMessage::new(
					DEFAULT_RETRIES,
					tx_request,
					EventMetadata::Relay(metadata),
				))
				.unwrap();
		} else {
			panic!(
				"{}]-[cccp-handler       ] Unknown chain ID received: {:?}",
				self.client.get_chain_name(),
				chain_id
			)
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

	async fn build_signatures(&self, mut msg: SocketMessage, is_inbound: bool) -> Signatures {
		let status = SocketEventStatus::from_u8(msg.status);
		if is_inbound {
			// build signatures for inbound requests
			match status {
				SocketEventStatus::Requested => Signatures::default(),
				SocketEventStatus::Executed => {
					msg.status = SocketEventStatus::Accepted.into();
					Signatures::from(self.sign_socket_message(msg).await)
				},
				SocketEventStatus::Reverted => {
					msg.status = SocketEventStatus::Rejected.into();
					Signatures::from(self.sign_socket_message(msg).await)
				},
				SocketEventStatus::Accepted | SocketEventStatus::Rejected =>
					self.get_signatures(msg).await,
				_ => panic!(
					"{}]-[cccp-handler       ] Unknown socket event status received: {:?}",
					self.client.get_chain_name(),
					status
				),
			}
		} else {
			// build signatures for outbound requests
			match status {
				SocketEventStatus::Requested => {
					msg.status = SocketEventStatus::Accepted.into();
					Signatures::from(self.sign_socket_message(msg).await)
				},
				SocketEventStatus::Accepted | SocketEventStatus::Rejected =>
					self.get_signatures(msg).await,
				SocketEventStatus::Executed | SocketEventStatus::Reverted => Signatures::default(),
				_ => panic!(
					"{}]-[cccp-handler       ] Unknown socket event status received: {:?}",
					self.client.get_chain_name(),
					status
				),
			}
		}
	}

	fn encode_socket_message(&self, msg: SocketMessage) -> Bytes {
		let req_id_token = Token::Tuple(vec![
			Token::FixedBytes(msg.req_id.chain.into()),
			Token::Uint(msg.req_id.round_id.into()),
			Token::Uint(msg.req_id.sequence.into()),
		]);
		let status_token = Token::Uint(msg.status.into());
		let ins_code_token = Token::Tuple(vec![
			Token::FixedBytes(msg.ins_code.chain.into()),
			Token::FixedBytes(msg.ins_code.method.into()),
		]);
		let params_token = Token::Tuple(vec![
			Token::FixedBytes(msg.params.token_idx0.into()),
			Token::FixedBytes(msg.params.token_idx1.into()),
			Token::Address(msg.params.refund),
			Token::Address(msg.params.to),
			Token::Uint(msg.params.amount),
			Token::Bytes(msg.params.variants.to_vec()),
		]);
		let msg_token =
			Token::Tuple(vec![req_id_token, status_token, ins_code_token, params_token]);
		encode(&[msg_token]).into()
	}

	async fn sign_socket_message(&self, msg: SocketMessage) -> Signature {
		self.client
			.wallet
			.signer
			.sign_message(self.encode_socket_message(msg))
			.await
			.unwrap()
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
	async fn send_socket_message(
		&self,
		msg: SocketMessage,
		metadata: RelayMetadata,
		is_inbound: bool,
	) {
		let status = SocketEventStatus::from_u8(msg.status);
		if Self::is_sequence_ended(status) {
			// do nothing if protocol sequence ended
			return
		}

		let relay_tx_chain_id = if is_inbound {
			self.get_inbound_relay_tx_chain_id(status, metadata.src_chain_id, metadata.dst_chain_id)
		} else {
			self.get_outbound_relay_tx_chain_id(
				status,
				metadata.src_chain_id,
				metadata.dst_chain_id,
			)
		};

		// build transaction request
		let to_socket = self
			.socket_contracts
			.iter()
			.find(|socket| socket.chain_id == relay_tx_chain_id)
			.unwrap()
			.address;
		// the original msg must be used for building calldata
		let origin_msg = msg.clone();
		let mut tx_request = TransactionRequest::new();
		let signatures = self.build_signatures(msg, is_inbound).await;
		tx_request =
			tx_request.data(self.build_poll_call_data(origin_msg, signatures)).to(to_socket);

		self.request_send_transaction(relay_tx_chain_id, tx_request, metadata.clone());

		log::info!(
			target: &self.client.get_chain_name(),
			"-[cccp-handler       ] 🔖 Request relay transaction to chain({:?}): {}",
			relay_tx_chain_id,
			metadata
		);
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
				"{}]-[cccp-handler       ] Unknown socket event status received: {:?}",
				self.client.get_chain_name(),
				status
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
				"{}]-[cccp-handler       ] Unknown socket event status received: {:?}",
				self.client.get_chain_name(),
				status
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
		matches!(
			(self.client.get_chain_id() == dst_chain_id, self.client.config.if_destination_chain),
			(true, BridgeDirection::Inbound) | (false, BridgeDirection::Outbound)
		)
	}
}
