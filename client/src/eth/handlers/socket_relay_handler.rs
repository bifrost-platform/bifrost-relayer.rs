use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use ethers::{
	abi::{ParamType, RawLog, Token},
	prelude::decode_logs,
	providers::JsonRpcClient,
	types::{Bytes, Filter, Log, Signature, TransactionReceipt, TransactionRequest, H256, U256},
};
use tokio::{sync::broadcast::Receiver, time::sleep};
use tokio_stream::StreamExt;

use br_primitives::{
	authority::RoundMetaData,
	bootstrap::{BootstrapHandler, BootstrapSharedData},
	constants::DEFAULT_BOOTSTRAP_ROUND_OFFSET,
	eth::{
		BootstrapState, BuiltRelayTransaction, ChainID, GasCoefficient, RecoveredSignature,
		RelayDirection, SocketEventStatus, SocketVariants, BOOTSTRAP_BLOCK_CHUNK_SIZE,
	},
	socket::{
		PollSubmit, RequestID, SerializedPoll, Signatures, SocketEvents, SocketMessage,
		SocketRelayBuilder,
	},
	sub_display_format, INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_CONTRACT_ABI,
};

use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler,
	SocketRelayMetadata, TxRequest,
};

use super::v2::V2Handler;

const SUB_LOG_TARGET: &str = "socket-handler";

/// The essential task that handles `socket relay` related events.
pub struct SocketRelayHandler<T> {
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The event senders that sends messages to the event channel. <chain_id, Arc<EventSender>>
	event_senders: BTreeMap<ChainID, Arc<EventSender>>,
	/// The block receiver that consumes new blocks from the block channel.
	block_receiver: Receiver<BlockMessage>,
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	system_clients: BTreeMap<ChainID, Arc<EthClient<T>>>,
	/// Signature of the `Socket` Event.
	socket_signature: H256,
	/// The 4 bytes selector of the `poll()` function.
	poll_selector: [u8; 4],
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

impl<T: JsonRpcClient> SocketRelayHandler<T> {
	/// Instantiates a new `SocketRelayHandler` instance.
	pub fn new(
		id: ChainID,
		event_channels: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		system_clients_vec: Vec<Arc<EthClient<T>>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Self {
		let mut system_clients = BTreeMap::new();
		system_clients_vec.iter().for_each(|client| {
			system_clients.insert(client.get_chain_id(), client.clone());
		});

		let client = system_clients.get(&id).expect(INVALID_CHAIN_ID).clone();

		let mut event_senders = BTreeMap::new();
		event_channels.iter().for_each(|event_sender| {
			event_senders.insert(event_sender.id, event_sender.clone());
		});

		Self {
			event_senders,
			block_receiver,
			socket_signature: client
				.protocol_contracts
				.socket
				.abi()
				.event("Socket")
				.expect(INVALID_CONTRACT_ABI)
				.signature(),
			poll_selector: client
				.protocol_contracts
				.socket
				.abi()
				.function("poll")
				.expect(INVALID_CONTRACT_ABI)
				.short_signature(),
			client,
			system_clients,
			bootstrap_shared_data,
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> V2Handler for SocketRelayHandler<T> {
	async fn process_confirmed_log(&self, _log: &Log, _is_bootstrap: bool) {}

	fn is_version2(&self, msg: &SocketMessage) -> bool {
		let v1_variants = Bytes::from_str("0x00").unwrap();
		let raw_variants = &msg.params.variants;
		if raw_variants == &v1_variants {
			return false;
		}
		true
	}

	fn decode_msg_variants(&self, raw_variants: &Bytes) -> SocketVariants {
		match ethers::abi::decode(
			&[ParamType::FixedBytes(4), ParamType::Address, ParamType::Uint(256), ParamType::Bytes],
			&raw_variants,
		) {
			Ok(variants) => {
				let mut result = SocketVariants::default();
				variants.into_iter().for_each(|variant| match variant {
					Token::FixedBytes(source_chain) => {
						result.source_chain = Bytes::from(source_chain);
					},
					Token::Address(sender) => {
						result.sender = sender;
					},
					Token::Uint(max_fee) => result.max_fee = max_fee,
					Token::Bytes(data) => {
						result.data = Bytes::from(data);
					},
					_ => {
						panic!(
							"[{}]-[{}]-[{}] Invalid Socket::variants received: {:?}",
							&self.client.get_chain_name(),
							SUB_LOG_TARGET,
							self.client.address(),
							raw_variants,
						);
					},
				});
				return result;
			},
			Err(error) => {
				panic!(
					"[{}]-[{}]-[{}] Invalid Socket::variants received: {:?}, Error: {}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					raw_variants,
					error
				)
			},
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for SocketRelayHandler<T> {
	async fn run(&mut self) {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapSocketRelay).await {
				self.bootstrap().await;

				sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let block_msg = self.block_receiver.recv().await.unwrap();

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 📦 Imported #{:?} with target logs({:?})",
					sub_display_format(SUB_LOG_TARGET),
					block_msg.block_number,
					block_msg.target_logs.len(),
				);

				let mut stream = tokio_stream::iter(block_msg.target_logs);
				while let Some(log) = stream.next().await {
					if self.is_target_contract(&log) && self.is_target_event(log.topics[0]) {
						Handler::process_confirmed_log(self, &log, false).await;
					}
				}
			}
		}
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) {
		if let Some(receipt) =
			self.client.get_transaction_receipt(log.transaction_hash.unwrap()).await
		{
			if receipt.status.unwrap().is_zero() && receipt.from == self.client.address() {
				// only handles owned transactions
				self.process_reverted_transaction(receipt).await;
				return;
			}
			match decode_logs::<SocketEvents>(&[RawLog::from(log.clone())]) {
				Ok(decoded) => match &decoded[0] {
					SocketEvents::Socket(socket) => {
						let mut metadata = SocketRelayMetadata::new(
							self.is_inbound_sequence(ChainID::from_be_bytes(
								socket.msg.ins_code.chain,
							)),
							SocketEventStatus::from_u8(socket.msg.status),
							socket.msg.req_id.sequence,
							ChainID::from_be_bytes(socket.msg.req_id.chain),
							ChainID::from_be_bytes(socket.msg.ins_code.chain),
							socket.msg.params.to,
						);
						if V2Handler::is_version2(self, &socket.msg) {
							// TODO: call execution_filter
							metadata.variants =
								V2Handler::decode_msg_variants(self, &socket.msg.params.variants);
							V2Handler::process_confirmed_log(self, log, is_bootstrap).await;
						}

						if !is_bootstrap {
							log::info!(
								target: &self.client.get_chain_name(),
								"-[{}] 🔖 Detected socket event: {}, {:?}-{:?}",
								sub_display_format(SUB_LOG_TARGET),
								metadata,
								log.block_number.unwrap(),
								log.transaction_hash,
							);
						}

						if !self.is_selected_relayer(socket.msg.req_id.round_id.into()).await {
							// do nothing if not selected
							return;
						}
						if self.is_sequence_ended(&socket.msg.req_id, metadata.src_chain_id).await {
							// do nothing if protocol sequence ended
							return;
						}

						self.send_socket_message(
							socket.msg.clone(),
							socket.msg.clone(),
							metadata.clone(),
							metadata.is_inbound,
						)
						.await;
					},
				},
				Err(error) => panic!(
					"[{}]-[{}]-[{}] Unknown error while decoding socket event: {:?}",
					self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					error,
				),
			}
		}
	}

	fn is_target_contract(&self, log: &Log) -> bool {
		if log.address == self.client.protocol_contracts.socket.address() {
			return true;
		}
		false
	}

	fn is_target_event(&self, topic: H256) -> bool {
		topic == self.socket_signature
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> SocketRelayBuilder for SocketRelayHandler<T> {
	fn build_poll_call_data(&self, msg: SocketMessage, sigs: Signatures) -> Bytes {
		let poll_submit = PollSubmit { msg, sigs, option: U256::default() };
		self.client.protocol_contracts.socket.poll(poll_submit).calldata().unwrap()
	}

	async fn build_transaction(
		&self,
		submit_msg: SocketMessage,
		sig_msg: SocketMessage,
		is_inbound: bool,
		relay_tx_chain_id: ChainID,
	) -> Option<BuiltRelayTransaction> {
		if let Some(system_client) = self.system_clients.get(&relay_tx_chain_id) {
			let to_socket = system_client.protocol_contracts.socket.address();
			// the original msg must be used for building calldata
			let (signatures, is_external) = if is_inbound {
				self.build_inbound_signatures(sig_msg).await
			} else {
				self.build_outbound_signatures(sig_msg).await
			};
			return Some(BuiltRelayTransaction::new(
				TransactionRequest::default()
					.data(self.build_poll_call_data(submit_msg, signatures))
					.to(to_socket),
				is_external,
			));
		}
		None
	}

	async fn build_inbound_signatures(&self, mut msg: SocketMessage) -> (Signatures, bool) {
		let status = SocketEventStatus::from_u8(msg.status);
		let mut is_external = false;
		let signatures = match status {
			SocketEventStatus::Requested | SocketEventStatus::Failed => Signatures::default(),
			SocketEventStatus::Executed => {
				msg.status = SocketEventStatus::Accepted.into();
				Signatures::from(self.sign_socket_message(msg))
			},
			SocketEventStatus::Reverted => {
				msg.status = SocketEventStatus::Rejected.into();
				Signatures::from(self.sign_socket_message(msg))
			},
			SocketEventStatus::Accepted | SocketEventStatus::Rejected => {
				is_external = true;
				self.get_sorted_signatures(msg).await
			},
			_ => panic!(
				"[{}]-[{}]-[{}] Unknown socket event status received: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address(),
				status
			),
		};
		(signatures, is_external)
	}

	async fn build_outbound_signatures(&self, mut msg: SocketMessage) -> (Signatures, bool) {
		let status = SocketEventStatus::from_u8(msg.status);
		let mut is_external = false;
		let signatures = match status {
			SocketEventStatus::Requested => {
				msg.status = SocketEventStatus::Accepted.into();
				Signatures::from(self.sign_socket_message(msg))
			},
			SocketEventStatus::Accepted | SocketEventStatus::Rejected => {
				is_external = true;
				self.get_sorted_signatures(msg).await
			},
			SocketEventStatus::Executed | SocketEventStatus::Reverted => Signatures::default(),
			_ => panic!(
				"[{}]-[{}]-[{}] Unknown socket event status received: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address(),
				status
			),
		};
		(signatures, is_external)
	}

	fn encode_socket_message(&self, msg: SocketMessage) -> Vec<u8> {
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

		ethers::abi::encode(&[Token::Tuple(vec![
			req_id_token,
			status_token,
			ins_code_token,
			params_token,
		])])
	}

	fn sign_socket_message(&self, msg: SocketMessage) -> Signature {
		let encoded_msg = self.encode_socket_message(msg);
		self.client.wallet.sign_message(&encoded_msg)
	}

	async fn get_sorted_signatures(&self, msg: SocketMessage) -> Signatures {
		let raw_sigs = self
			.client
			.contract_call(
				self.client
					.protocol_contracts
					.socket
					.get_signatures(msg.clone().req_id, msg.clone().status),
				"socket.get_signatures",
			)
			.await;

		let raw_concated_v = &raw_sigs.v.to_string()[2..];

		let mut recovered_sigs = vec![];
		let encoded_msg = self.encode_socket_message(msg);
		for idx in 0..raw_sigs.r.len() {
			let sig = Signature {
				r: raw_sigs.r[idx].into(),
				s: raw_sigs.s[idx].into(),
				v: u64::from_str_radix(&raw_concated_v[idx * 2..idx * 2 + 2], 16).unwrap(),
			};
			recovered_sigs.push(RecoveredSignature::new(
				idx,
				sig,
				self.client.wallet.recover_message(sig, &encoded_msg),
			));
		}
		recovered_sigs.sort_by_key(|k| k.signer);

		let mut sorted_sigs = Signatures::default();
		let mut sorted_concated_v = String::from("0x");
		recovered_sigs.into_iter().for_each(|sig| {
			let idx = sig.idx;
			sorted_sigs.r.push(raw_sigs.r[idx]);
			sorted_sigs.s.push(raw_sigs.s[idx]);
			let v = Bytes::from([sig.signature.v as u8]);
			sorted_concated_v.push_str(&v.to_string()[2..]);
		});
		sorted_sigs.v = Bytes::from_str(&sorted_concated_v).unwrap();

		sorted_sigs
	}
}

impl<T: JsonRpcClient> SocketRelayHandler<T> {
	/// (Re-)Handle the reverted relay transaction. This method only handles if it was an
	/// Inbound-Requested or Outbound-Accepted sequence. This will let the sequence follow the
	/// fail-case flow.
	async fn process_reverted_transaction(&self, receipt: TransactionReceipt) {
		if let Some(tx) = self.client.get_transaction(receipt.transaction_hash).await {
			// the reverted transaction must be execution of `poll()`
			let tx_selector = &tx.input[0..4];
			if tx_selector == self.poll_selector {
				match self
					.client
					.protocol_contracts
					.socket
					.decode_with_selector::<SerializedPoll, Bytes>(self.poll_selector, tx.input)
				{
					Ok(poll) => {
						let mut submit_msg = poll.msg.clone();
						let sig_msg = poll.msg;
						let prev_status = SocketEventStatus::from_u8(sig_msg.status);
						let mut metadata = SocketRelayMetadata::new(
							self.is_inbound_sequence(ChainID::from_be_bytes(
								sig_msg.ins_code.chain,
							)),
							SocketEventStatus::from_u8(submit_msg.status),
							sig_msg.req_id.sequence,
							ChainID::from_be_bytes(sig_msg.req_id.chain),
							ChainID::from_be_bytes(sig_msg.ins_code.chain),
							sig_msg.params.to,
						);

						if metadata.is_inbound
							&& matches!(prev_status, SocketEventStatus::Requested)
						{
							// if inbound-Requested
							submit_msg.status = SocketEventStatus::Failed.into();
						} else if !metadata.is_inbound
							&& matches!(prev_status, SocketEventStatus::Accepted)
						{
							// if outbound-Accepted
							submit_msg.status = SocketEventStatus::Rejected.into();
						} else {
							// do not re-process
							return;
						}
						metadata.status = SocketEventStatus::from_u8(submit_msg.status);

						log::info!(
							target: &self.client.get_chain_name(),
							"-[{}] ♻️  Re-Processed reverted relay transaction: {}, Reverted at: {:?}-{:?}",
							sub_display_format(SUB_LOG_TARGET),
							metadata,
							receipt.block_number.unwrap(),
							receipt.transaction_hash,
						);

						self.send_socket_message(
							submit_msg,
							sig_msg,
							metadata.clone(),
							metadata.is_inbound,
						)
						.await;
					},
					Err(error) => {
						// ignore for now if function input data decode fails
						log::warn!(
							target: &self.client.get_chain_name(),
							"-[{}] ⚠️  Tried to re-process the reverted relay transaction but failed to decode function input: {}, Reverted at: {:?}-{:?}",
							sub_display_format(SUB_LOG_TARGET),
							error.to_string(),
							receipt.block_number.unwrap(),
							receipt.transaction_hash,
						);
						sentry::capture_message(
								format!(
									"[{}]-[{}]-[{}] ⚠️  Tried to re-process the reverted relay transaction but failed to decode function input: {}, Reverted at: {:?}-{:?}",
									&self.client.get_chain_name(),
									SUB_LOG_TARGET,
									self.client.address(),
									error,
									receipt.block_number.unwrap(),
									receipt.transaction_hash,
								)
								.as_str(),
								sentry::Level::Warning,
							);
					},
				}
			}
		}
	}

	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(
		&self,
		submit_msg: SocketMessage,
		sig_msg: SocketMessage,
		metadata: SocketRelayMetadata,
		is_inbound: bool,
	) {
		let status = SocketEventStatus::from_u8(submit_msg.status);

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
		if let Some(built_transaction) =
			self.build_transaction(submit_msg, sig_msg, is_inbound, relay_tx_chain_id).await
		{
			self.request_send_transaction(
				relay_tx_chain_id,
				built_transaction.tx_request,
				metadata,
				built_transaction.is_external,
				if built_transaction.is_external {
					GasCoefficient::Low
				} else {
					GasCoefficient::Mid
				},
			);
		}
	}

	/// Get the chain ID of the inbound sequence relay transaction.
	fn get_inbound_relay_tx_chain_id(
		&self,
		status: SocketEventStatus,
		src_chain_id: ChainID,
		dst_chain_id: ChainID,
	) -> ChainID {
		match status {
			SocketEventStatus::Requested
			| SocketEventStatus::Failed
			| SocketEventStatus::Executed
			| SocketEventStatus::Reverted => dst_chain_id,
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
		src_chain_id: ChainID,
		dst_chain_id: ChainID,
	) -> ChainID {
		match status {
			SocketEventStatus::Requested
			| SocketEventStatus::Executed
			| SocketEventStatus::Reverted => src_chain_id,
			SocketEventStatus::Accepted | SocketEventStatus::Rejected => dst_chain_id,
			_ => panic!(
				"[{}]-[{}] Unknown socket event status received: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				status
			),
		}
	}

	/// Compare the request status recorded in source chain with event status to determine if the
	/// event has already been committed or rollbacked.
	async fn is_sequence_ended(&self, rid: &RequestID, src_chain_id: ChainID) -> bool {
		if let Some(src_client) = &self.system_clients.get(&src_chain_id) {
			let request = src_client
				.contract_call(
					src_client.protocol_contracts.socket.get_request(rid.clone()),
					"socket.get_request",
				)
				.await;

			return matches!(
				SocketEventStatus::from_u8(request.field[0].clone().into()),
				SocketEventStatus::Committed | SocketEventStatus::Rollbacked
			);
		}
		false
	}

	/// Verifies whether the socket event is an inbound sequence.
	fn is_inbound_sequence(&self, dst_chain_id: ChainID) -> bool {
		matches!(
			(self.client.get_chain_id() == dst_chain_id, self.client.metadata.if_destination_chain),
			(true, RelayDirection::Inbound) | (false, RelayDirection::Outbound)
		)
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self, round: U256) -> bool {
		if self.client.metadata.is_native {
			let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
			return self
				.client
				.contract_call(
					relayer_manager.is_previous_selected_relayer(
						round,
						self.client.address(),
						false,
					),
					"relayer_manager.is_previous_selected_relayer",
				)
				.await;
		} else if let Some((_id, native_client)) =
			self.system_clients.iter().find(|(_id, client)| client.metadata.is_native)
		{
			// always use the native client's contract. due to handle missed VSP's.
			let relayer_manager =
				native_client.protocol_contracts.relayer_manager.as_ref().unwrap();
			return native_client
				.contract_call(
					relayer_manager.is_previous_selected_relayer(
						round,
						self.client.address(),
						false,
					),
					"relayer_manager.is_previous_selected_relayer",
				)
				.await;
		}
		false
	}

	/// Request send socket relay transaction to the target event channel.
	fn request_send_transaction(
		&self,
		chain_id: ChainID,
		tx_request: TransactionRequest,
		metadata: SocketRelayMetadata,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		if let Some(event_sender) = self.event_senders.get(&chain_id) {
			match event_sender.send(EventMessage::new(
				TxRequest::Legacy(tx_request),
				EventMetadata::SocketRelay(metadata.clone()),
				true,
				give_random_delay,
				gas_coefficient,
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
					sentry::capture_message(
						format!(
							"[{}]-[{}]-[{}] ❗️ Failed to send relay transaction to chain({:?}): {}, Error: {}",
							&self.client.get_chain_name(),
							SUB_LOG_TARGET,
							self.client.address(),
							chain_id,
							metadata,
							error
						)
						.as_str(),
						sentry::Level::Error,
					);
				},
			}
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for SocketRelayHandler<T> {
	async fn bootstrap(&self) {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping Socket events.",
			sub_display_format(SUB_LOG_TARGET),
		);

		let logs = self.get_bootstrap_events().await;

		let mut stream = tokio_stream::iter(logs);
		while let Some(log) = stream.next().await {
			Handler::process_confirmed_log(self, &log, true).await;
		}

		let mut bootstrap_count = self.bootstrap_shared_data.socket_bootstrap_count.lock().await;
		*bootstrap_count += 1;

		// If All thread complete the task, starts the blockManager
		if *bootstrap_count == self.system_clients.len() as u8 {
			let mut bootstrap_guard = self.bootstrap_shared_data.bootstrap_states.write().await;

			for state in bootstrap_guard.iter_mut() {
				*state = BootstrapState::NormalStart;
			}

			log::info!(
				target: "bifrost-relayer",
				"-[{}] ⚙️  [Bootstrap mode] Bootstrap process successfully ended.",
				sub_display_format(SUB_LOG_TARGET),
			);
		}
	}

	async fn get_bootstrap_events(&self) -> Vec<Log> {
		let mut logs = vec![];

		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			let round_info: RoundMetaData = if self.client.metadata.is_native {
				self.client
					.contract_call(
						self.client.protocol_contracts.authority.round_info(),
						"authority.round_info",
					)
					.await
			} else if let Some((_id, native_client)) =
				self.system_clients.iter().find(|(_id, client)| client.metadata.is_native)
			{
				native_client
					.contract_call(
						native_client.protocol_contracts.authority.round_info(),
						"authority.round_info",
					)
					.await
			} else {
				panic!(
					"[{}]-[{}] {}",
					self.client.get_chain_name(),
					SUB_LOG_TARGET,
					INVALID_BIFROST_NATIVENESS,
				);
			};

			let bootstrap_offset_height = self
				.client
				.get_bootstrap_offset_height_based_on_block_time(
					bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
					round_info,
				)
				.await;

			let latest_block_number = self.client.get_latest_block_number().await;
			let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
			let to_block = latest_block_number;

			// Split from_block into smaller chunks
			while from_block <= to_block {
				let chunk_to_block =
					std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

				let filter = Filter::new()
					.address(self.client.protocol_contracts.socket.address())
					.topic0(self.socket_signature)
					.from_block(from_block)
					.to_block(chunk_to_block);
				let target_logs_chunk = self.client.get_logs(&filter).await;
				logs.extend(target_logs_chunk);

				from_block = chunk_to_block + 1;
			}
		}

		logs
	}

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}

#[cfg(test)]
mod tests {
	use std::{str::FromStr, sync::Arc};

	use ethers::{
		providers::{Http, Provider},
		types::H160,
	};

	use br_primitives::socket::SocketContract;

	use super::*;

	#[tokio::test]
	async fn test_is_already_done() {
		let provider = Arc::new(Provider::<Http>::try_from("").unwrap());

		let target_socket = SocketContract::new(
			H160::from_str("0x0218371b18340aBD460961bdF3Bd5F01858dAB53").unwrap(),
			provider.clone(),
		);

		let data = r#"{
				"chain": [0,0,191,192],
				"round_id": 3588,
				"sequence": 359
			}"#;

		let request_id: RequestID = serde_json::from_str(&data).unwrap();
		println!("request_id : {:?}", request_id);

		let request = target_socket.get_request(request_id).call().await.unwrap();
		println!("request : {:?}", request);
	}

	#[test]
	fn test_socket_event_decode() {
		let topic =
			H256::from_str("0x918454f530580823dd0d8cf59cacb45a6eb7cc62f222d7129efba5821e77f191")
				.unwrap();
		let data = Bytes::from_str("0x00000000000000000000000000000000000000000000000000000000000000200000bfc0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001328000000000000000000000000000000000000000000000000000000000000037100000000000000000000000000000000000000000000000000000000000000010000006100000000000000000000000000000000000000000000000000000000030203010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e000000008000000030000bfc0c67f0b7c01f6888d43b563b3a8b851856bcfab64000000000000000000000000000000000000000000000000000000000000000000000000000000000000000074345eef74bc9a92d09012ddbcc8f0443f92966f00000000000000000000000074345eef74bc9a92d09012ddbcc8f0443f92966f000000000000000000000000000000000000000000000001143cc409ee26800000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000023078000000000000000000000000000000000000000000000000000000000000").unwrap();

		println!("topic -> {:?}", topic);
		println!("data -> {:?}", data.clone());

		match ethers::abi::decode(
			&[ParamType::Tuple(vec![
				ParamType::Tuple(vec![
					ParamType::FixedBytes(4),
					ParamType::Uint(64),
					ParamType::Uint(128),
				]),
				ParamType::Uint(8),
				ParamType::Tuple(vec![ParamType::FixedBytes(4), ParamType::FixedBytes(16)]),
				ParamType::Tuple(vec![
					ParamType::FixedBytes(32),
					ParamType::FixedBytes(32),
					ParamType::Address,
					ParamType::Address,
					ParamType::Uint(256),
					ParamType::Bytes,
				]),
			])],
			&data,
		) {
			Ok(socket) => println!("socket -> {:?}", socket[0].to_string()),
			Err(error) => {
				panic!("decode failed -> {}", error);
			},
		}
	}

	#[test]
	fn test_socket_variants_decode() {
		println!("default -> {}", Bytes::default());
		println!("zero -> {}", Bytes::from_str("0x00").unwrap());
		let raw_variants = Bytes::from_str("0x00000005000000000000000000000000000000000000000000000000000000000000000000000000000000005b38da6a701c568545dcfcb03fcb875f56beddc400000000000000000000000000000000000000000000000000000000000001c8000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000012300000000000000000000000000000000000000000000000000000000000000").unwrap();
		match ethers::abi::decode(
			&[ParamType::FixedBytes(4), ParamType::Address, ParamType::Uint(256), ParamType::Bytes],
			&raw_variants,
		) {
			Ok(variants) => {
				log::info!("variants -> {:?}", variants);
				let mut result = SocketVariants::default();
				variants.into_iter().for_each(|variant| match variant {
					Token::FixedBytes(source_chain) => {
						result.source_chain = Bytes::from(source_chain);
					},
					Token::Address(sender) => {
						result.sender = sender;
					},
					Token::Uint(max_fee) => result.max_fee = max_fee,
					Token::Bytes(data) => {
						result.data = Bytes::from(data);
					},
					_ => {
						panic!("decode failed");
					},
				});
				println!("variants -> {:?}", result);
			},
			Err(error) => {
				panic!("decode failed -> {}", error);
			},
		}
	}
}
