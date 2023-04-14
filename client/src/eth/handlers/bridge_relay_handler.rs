use std::sync::Arc;

use cccp_primitives::{
	contracts::socket_external::{
		BridgeRelayBuilder, PollSubmit, Signatures, SocketEvents, SocketExternal, SocketMessage,
	},
	eth::{BridgeDirection, Contract, SocketEventStatus},
	socket_external::SerializedPoll,
	sub_display_format,
};
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
	BlockMessage, BridgeRelayMetadata, EthClient, EventMessage, EventMetadata, EventSender,
	Handler, DEFAULT_RETRIES,
};

const SUB_LOG_TARGET: &str = "bridge-handler";

/// The essential task that handles `bridge relay` related events.
pub struct BridgeRelayHandler<T> {
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
	/// Signature of the `Socket` Event.
	pub socket_signature: H256,
}

impl<T: JsonRpcClient> BridgeRelayHandler<T> {
	/// Instantiates a new `BridgeRelayHandler` instance.
	pub fn new(
		event_senders: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		client: Arc<EthClient<T>>,
		target_contract: H160,
		target_socket: H160,
		socket_contracts: Vec<Contract>,
	) -> Self {
		let target_socket = SocketExternal::new(target_socket, client.provider.clone());
		let socket_signature = target_socket.abi().event("Socket").unwrap().signature();
		Self {
			event_senders,
			block_receiver,
			client,
			target_contract,
			target_socket,
			socket_contracts,
			socket_signature,
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for BridgeRelayHandler<T> {
	async fn run(&mut self) {
		loop {
			let block_msg = self.block_receiver.recv().await.unwrap();

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ✨ Imported #{:?} ({}) with target transactions({:?})",
				sub_display_format(SUB_LOG_TARGET),
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
			let status = receipt.status.unwrap();
			if status.is_zero() {
				self.process_reverted_transaction(receipt).await;
				return
			}

			let mut stream = tokio_stream::iter(receipt.logs);

			while let Some(log) = stream.next().await {
				if self.is_target_event(log.topics[0]) {
					let raw_log: RawLog = log.clone().into();
					match decode_logs::<SocketEvents>(&[raw_log]) {
						Ok(decoded) => match &decoded[0] {
							SocketEvents::Socket(socket) => {
								let status = SocketEventStatus::from_u8(socket.msg.status);
								let src_chain_id = u32::from_be_bytes(socket.msg.req_id.chain);
								let dst_chain_id = u32::from_be_bytes(socket.msg.ins_code.chain);
								let is_inbound = self.is_inbound_sequence(dst_chain_id);

								let metadata = BridgeRelayMetadata::new(
									if is_inbound {
										"Inbound".to_string()
									} else {
										"Outbound".to_string()
									},
									status,
									socket.msg.req_id.sequence,
									src_chain_id,
									dst_chain_id,
								);

								log::info!(
									target: &self.client.get_chain_name(),
									"-[{}] 🔖 Detected socket event: {}, {:?}-{:?}",
									sub_display_format(SUB_LOG_TARGET),
									metadata,
									receipt.block_number.unwrap(),
									receipt.transaction_hash,
								);

								self.send_socket_message(socket.msg.clone(), metadata, is_inbound)
									.await;
							},
						},
						Err(error) => panic!(
							"[{}]-[{}] Unknown error while decoding socket event: {}",
							self.client.get_chain_name(),
							SUB_LOG_TARGET,
							error
						),
					}
				}
			}
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

	fn is_target_event(&self, topic: H256) -> bool {
		topic == self.socket_signature
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BridgeRelayBuilder for BridgeRelayHandler<T> {
	fn build_poll_call_data(&self, msg: SocketMessage, sigs: Signatures) -> Bytes {
		let poll_submit = PollSubmit { msg, sigs, option: U256::default() };
		self.target_socket.poll(poll_submit).calldata().unwrap()
	}

	async fn build_transaction(
		&self,
		msg: SocketMessage,
		is_inbound: bool,
		relay_tx_chain_id: u32,
	) -> TransactionRequest {
		// build transaction request
		let to_socket = self
			.socket_contracts
			.iter()
			.find(|socket| socket.chain_id == relay_tx_chain_id)
			.unwrap()
			.address;
		// the original msg must be used for building calldata
		let origin_msg = msg.clone();
		let tx_request = TransactionRequest::default();
		let signatures = self.build_signatures(msg, is_inbound).await;
		tx_request.data(self.build_poll_call_data(origin_msg, signatures)).to(to_socket)
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
					"[{}]-[{}] Unknown socket event status received: {:?}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
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
					"[{}]-[{}] Unknown socket event status received: {:?}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
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

impl<T: JsonRpcClient> BridgeRelayHandler<T> {
	/// (Re-)Handle the reverted relay transaction. This method only handles if it was an
	/// Inbound-Requested or Outbound-Accepted sequence. This will let the sequence follow the
	/// fail-case flow.
	async fn process_reverted_transaction(&self, receipt: TransactionReceipt) {
		// only handles owned transactions
		if self.is_owned_relay_transaction(&receipt) {
			if let Some(tx) = self.client.get_transaction(receipt.transaction_hash).await.unwrap() {
				// the reverted transaction must be execution of `poll()`
				let selector = &tx.input[0..4];
				let poll_selector =
					self.target_socket.abi().function("poll").unwrap().short_signature();
				if selector == poll_selector {
					match self
						.target_socket
						.decode_with_selector::<SerializedPoll, Bytes>(poll_selector, tx.input)
					{
						Ok(mut poll) => {
							let prev_status = SocketEventStatus::from_u8(poll.msg.status);
							let src_chain_id = u32::from_be_bytes(poll.msg.req_id.chain);
							let dst_chain_id = u32::from_be_bytes(poll.msg.ins_code.chain);
							let is_inbound = self.is_inbound_sequence(dst_chain_id);

							if is_inbound && matches!(prev_status, SocketEventStatus::Requested) {
								// if inbound-Requested
								poll.msg.status = SocketEventStatus::Failed.into();
							} else if !is_inbound &&
								matches!(prev_status, SocketEventStatus::Accepted)
							{
								// if outbound-Accepted
								poll.msg.status = SocketEventStatus::Rejected.into();
							} else {
								return
							}

							let metadata = BridgeRelayMetadata::new(
								if is_inbound {
									"Inbound".to_string()
								} else {
									"Outbound".to_string()
								},
								SocketEventStatus::from_u8(poll.msg.status),
								poll.msg.req_id.sequence,
								src_chain_id,
								dst_chain_id,
							);

							log::info!(
								target: &self.client.get_chain_name(),
								"-[{}] ♻️ Re-Processed reverted relay transaction: {}, Reverted at: {:?}-{:?}",
								sub_display_format(SUB_LOG_TARGET),
								metadata,
								receipt.block_number.unwrap(),
								receipt.transaction_hash,
							);

							self.send_socket_message(poll.msg, metadata, is_inbound).await;
						},
						Err(_) => {
							// ignore for now if function input data decode fails
						},
					}
				}
			}
		}
	}

	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(
		&self,
		msg: SocketMessage,
		metadata: BridgeRelayMetadata,
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

		// build and send transaction request
		let tx_request = self.build_transaction(msg, is_inbound, relay_tx_chain_id).await;
		self.request_send_transaction(relay_tx_chain_id, tx_request, metadata);
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
				"[{}]-[{}] Unknown socket event status received: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
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
				"[{}]-[{}] Unknown socket event status received: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
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

	/// Verifies whether the current relayer owns the relay transaction.
	fn is_owned_relay_transaction(&self, receipt: &TransactionReceipt) -> bool {
		ethers::utils::to_checksum(&receipt.from, None) ==
			ethers::utils::to_checksum(&self.client.address(), None)
	}

	/// Request send bridge relay transaction to the target event channel.
	fn request_send_transaction(
		&self,
		chain_id: u32,
		tx_request: TransactionRequest,
		metadata: BridgeRelayMetadata,
	) {
		if let Some(event_sender) =
			self.event_senders.iter().find(|event_sender| event_sender.id == chain_id)
		{
			match event_sender.sender.send(EventMessage::new(
				DEFAULT_RETRIES,
				tx_request,
				EventMetadata::BridgeRelay(metadata.clone()),
				true,
			)) {
				Ok(()) => log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔖 Request relay transaction to chain({:?}): {}",
					sub_display_format(SUB_LOG_TARGET),
					chain_id,
					metadata
				),
				Err(error) => {
					log::error!(
						target: &self.client.get_chain_name(),
						"-[{}] ❗️ Failed to send relay transaction to chain({:?}): {}, Error: {}",
						sub_display_format(SUB_LOG_TARGET),
						chain_id,
						metadata,
						error.to_string()
					);
					sentry::capture_error(&error);
				},
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cccp_primitives::socket_external::SerializedPoll;
	use ethers::{
		providers::{Http, Middleware, Provider},
		types::H160,
	};
	use std::{str::FromStr, sync::Arc};

	#[tokio::test]
	async fn function_decode() {
		let provider = Arc::new(Provider::<Http>::try_from("").unwrap());

		let target_socket = SocketExternal::new(
			H160::from_str("0x0218371b18340aBD460961bdF3Bd5F01858dAB53").unwrap(),
			provider.clone(),
		);
		let poll_selector = target_socket.abi().function("poll").unwrap().short_signature();

		let tx = provider
			.get_transaction(
				H256::from_str(
					"0xbef5fc7d225eb4835f030fc5ccb8b9fe12c33943b89a5588aa6b618d796c8b80",
				)
				.unwrap(),
			)
			.await
			.unwrap();

		if let Some(tx) = tx {
			match target_socket
				.decode_with_selector::<SerializedPoll, Bytes>(poll_selector, tx.input)
			{
				Ok(mut poll) => {
					let status = SocketEventStatus::from_u8(poll.msg.status);
					let src_chain_id = u32::from_be_bytes(poll.msg.req_id.chain);
					let dst_chain_id = u32::from_be_bytes(poll.msg.ins_code.chain);
					let is_inbound = true;

					if is_inbound && matches!(status, SocketEventStatus::Requested) {
						// if inbound-Requested
						poll.msg.status = SocketEventStatus::Failed.into();
					} else if !is_inbound && matches!(status, SocketEventStatus::Accepted) {
						// if outbound-Accepted
						poll.msg.status = SocketEventStatus::Rejected.into();
					} else {
						println!("failed");
						return
					}

					let metadata = BridgeRelayMetadata::new(
						if is_inbound { "Inbound".to_string() } else { "Outbound".to_string() },
						SocketEventStatus::from_u8(poll.msg.status),
						poll.msg.req_id.sequence,
						src_chain_id,
						dst_chain_id,
					);
					println!("{}", metadata);
				},
				Err(error) => {
					println!("error -> {:?}", error);
				},
			}
		}
	}
}
