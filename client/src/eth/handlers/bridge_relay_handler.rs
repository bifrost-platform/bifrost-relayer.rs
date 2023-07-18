use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use ethers::{
	abi::{RawLog, Token},
	prelude::decode_logs,
	providers::JsonRpcClient,
	types::{Bytes, Filter, Log, Signature, TransactionReceipt, TransactionRequest, H256, U256},
};
use tokio::{
	sync::{broadcast::Receiver, Mutex, RwLock},
	time::sleep,
};
use tokio_stream::StreamExt;

use br_primitives::{
	authority::RoundMetaData,
	cli::BootstrapConfig,
	eth::{
		BootstrapState, BridgeDirection, ChainID, GasCoefficient, RecoveredSignature,
		SocketEventStatus, BOOTSTRAP_BLOCK_CHUNK_SIZE,
	},
	socket::{
		BridgeRelayBuilder, PollSubmit, RequestID, SerializedPoll, Signatures, SocketEvents,
		SocketMessage,
	},
	sub_display_format, INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_CONTRACT_ABI,
};

use crate::eth::{
	BlockMessage, BridgeRelayMetadata, EthClient, EventMessage, EventMetadata, EventSender,
	Handler, TxRequest,
};

use super::BootstrapHandler;

const SUB_LOG_TARGET: &str = "bridge-handler";

/// The essential task that handles `bridge relay` related events.
pub struct BridgeRelayHandler<T> {
	/// The event senders that sends messages to the event channel. <chain_id, Arc<EventSender>>
	pub event_senders: BTreeMap<ChainID, Arc<EventSender>>,
	/// The block receiver that consumes new blocks from the block channel.
	pub block_receiver: Receiver<BlockMessage>,
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	pub system_clients: BTreeMap<ChainID, Arc<EthClient<T>>>,
	/// Signature of the `Socket` Event.
	pub socket_signature: H256,
	/// Completion of bootstrapping
	pub bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
	/// Completion of bootstrapping count
	pub bootstrapping_count: Arc<Mutex<u8>>,
	/// Bootstrap config
	pub bootstrap_config: BootstrapConfig,
}

impl<T: JsonRpcClient> BridgeRelayHandler<T> {
	/// Instantiates a new `BridgeRelayHandler` instance.
	pub fn new(
		id: ChainID,
		event_channels: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		system_clients_vec: Vec<Arc<EthClient<T>>>,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
		bootstrapping_count: Arc<Mutex<u8>>,
		bootstrap_config: BootstrapConfig,
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
				.socket
				.abi()
				.event("Socket")
				.expect(INVALID_CONTRACT_ABI)
				.signature(),
			client,
			system_clients,
			bootstrap_states,
			bootstrapping_count,
			bootstrap_config,
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for BridgeRelayHandler<T> {
	async fn run(&mut self) {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapBridgeRelay).await {
				self.bootstrap().await;

				sleep(Duration::from_millis(self.client.call_interval)).await;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let block_msg = self.block_receiver.recv().await.unwrap();

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 📦 Imported #{:?} ({}) with target logs({:?})",
					sub_display_format(SUB_LOG_TARGET),
					block_msg.block_number,
					block_msg.block_hash,
					block_msg.target_logs.len(),
				);

				let mut stream = tokio_stream::iter(block_msg.target_logs);
				while let Some(log) = stream.next().await {
					self.process_confirmed_log(&log, false).await;
				}
			}
		}
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) {
		if self.is_target_contract(log) {
			if let Some(receipt) =
				self.client.get_transaction_receipt(log.transaction_hash.unwrap()).await
			{
				if receipt.status.unwrap().is_zero() {
					self.process_reverted_transaction(receipt).await;
				}

				if self.is_target_event(log.topics[0]) {
					let raw_log = RawLog::from(log.clone());
					match decode_logs::<SocketEvents>(&[raw_log]) {
						Ok(decoded) => match &decoded[0] {
							SocketEvents::Socket(socket) => {
								let status = SocketEventStatus::from_u8(socket.msg.status);
								let src_chain_id = ChainID::from_be_bytes(socket.msg.req_id.chain);
								let dst_chain_id =
									ChainID::from_be_bytes(socket.msg.ins_code.chain);
								let is_inbound = self.is_inbound_sequence(dst_chain_id);

								let metadata = BridgeRelayMetadata::new(
									is_inbound,
									status,
									socket.msg.req_id.sequence,
									src_chain_id,
									dst_chain_id,
								);

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

								if Self::is_sequence_ended(status) ||
									self.is_already_done(&socket.msg.req_id, src_chain_id).await
								{
									// do nothing if protocol sequence ended
									return
								}
								if !self
									.is_selected_relayer(socket.msg.req_id.round_id.into())
									.await
								{
									// do nothing if not verified
									return
								}

								self.send_socket_message(
									socket.msg.clone(),
									socket.msg.clone(),
									metadata,
									is_inbound,
								)
								.await;
							},
						},
						Err(error) => panic!(
							"[{}]-[{}] Unknown error while decoding socket event: {:?}",
							self.client.get_chain_name(),
							SUB_LOG_TARGET,
							error,
						),
					}
				}
			}
		}
	}

	fn is_target_contract(&self, log: &Log) -> bool {
		if log.address == self.client.socket.address() || log.address == self.client.vault.address()
		{
			return true
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
		self.client.socket.poll(poll_submit).calldata().unwrap()
	}

	async fn build_transaction(
		&self,
		submit_msg: SocketMessage,
		sig_msg: SocketMessage,
		is_inbound: bool,
		relay_tx_chain_id: ChainID,
	) -> (TransactionRequest, bool) {
		let to_socket = self
			.system_clients
			.get(&relay_tx_chain_id)
			.expect(INVALID_CHAIN_ID)
			.socket
			.address();

		// the original msg must be used for building calldata
		let (signatures, is_external) = self.build_signatures(sig_msg, is_inbound).await;
		(
			TransactionRequest::default()
				.data(self.build_poll_call_data(submit_msg, signatures))
				.to(to_socket),
			is_external,
		)
	}

	async fn build_signatures(
		&self,
		mut msg: SocketMessage,
		is_inbound: bool,
	) -> (Signatures, bool) {
		let status = SocketEventStatus::from_u8(msg.status);
		let mut is_external = false;
		let signatures = if is_inbound {
			// build signatures for inbound requests
			match status {
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
					Signatures::from(self.sign_socket_message(msg))
				},
				SocketEventStatus::Accepted | SocketEventStatus::Rejected => {
					is_external = true;
					self.get_sorted_signatures(msg).await
				},
				SocketEventStatus::Executed | SocketEventStatus::Reverted => Signatures::default(),
				_ => panic!(
					"[{}]-[{}] Unknown socket event status received: {:?}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					status
				),
			}
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
				self.client.socket.get_signatures(msg.clone().req_id, msg.clone().status),
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

impl<T: JsonRpcClient> BridgeRelayHandler<T> {
	/// (Re-)Handle the reverted relay transaction. This method only handles if it was an
	/// Inbound-Requested or Outbound-Accepted sequence. This will let the sequence follow the
	/// fail-case flow.
	async fn process_reverted_transaction(&self, receipt: TransactionReceipt) {
		// only handles owned transactions
		if receipt.from == self.client.address() {
			if let Some(tx) = self.client.get_transaction(receipt.transaction_hash).await {
				// the reverted transaction must be execution of `poll()`
				let selector = &tx.input[0..4];
				let poll_selector = self
					.client
					.socket
					.abi()
					.function("poll")
					.expect(INVALID_CONTRACT_ABI)
					.short_signature();
				if selector == poll_selector {
					match self
						.client
						.socket
						.decode_with_selector::<SerializedPoll, Bytes>(poll_selector, tx.input)
					{
						Ok(poll) => {
							let prev_status = SocketEventStatus::from_u8(poll.msg.status);
							let src_chain_id = ChainID::from_be_bytes(poll.msg.req_id.chain);
							let dst_chain_id = ChainID::from_be_bytes(poll.msg.ins_code.chain);
							let is_inbound = self.is_inbound_sequence(dst_chain_id);

							let mut submit_msg = poll.msg.clone();
							let sig_msg = poll.msg;

							if is_inbound && matches!(prev_status, SocketEventStatus::Requested) {
								// if inbound-Requested
								submit_msg.status = SocketEventStatus::Failed.into();
							} else if !is_inbound &&
								matches!(prev_status, SocketEventStatus::Accepted)
							{
								// if outbound-Accepted
								submit_msg.status = SocketEventStatus::Rejected.into();
							} else {
								return
							}

							let metadata = BridgeRelayMetadata::new(
								is_inbound,
								SocketEventStatus::from_u8(submit_msg.status),
								sig_msg.req_id.sequence,
								src_chain_id,
								dst_chain_id,
							);

							log::info!(
								target: &self.client.get_chain_name(),
								"-[{}] ♻️  Re-Processed reverted relay transaction: {}, Reverted at: {:?}-{:?}",
								sub_display_format(SUB_LOG_TARGET),
								metadata,
								receipt.block_number.unwrap(),
								receipt.transaction_hash,
							);

							self.send_socket_message(submit_msg, sig_msg, metadata, is_inbound)
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
	}

	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(
		&self,
		submit_msg: SocketMessage,
		sig_msg: SocketMessage,
		metadata: BridgeRelayMetadata,
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
		let (tx_request, is_external) =
			self.build_transaction(submit_msg, sig_msg, is_inbound, relay_tx_chain_id).await;
		self.request_send_transaction(
			relay_tx_chain_id,
			tx_request,
			metadata,
			is_external,
			if is_external { GasCoefficient::Low } else { GasCoefficient::Mid },
		);
	}

	/// Get the chain ID of the inbound sequence relay transaction.
	fn get_inbound_relay_tx_chain_id(
		&self,
		status: SocketEventStatus,
		src_chain_id: ChainID,
		dst_chain_id: ChainID,
	) -> ChainID {
		match status {
			SocketEventStatus::Requested |
			SocketEventStatus::Failed |
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
		src_chain_id: ChainID,
		dst_chain_id: ChainID,
	) -> ChainID {
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
	fn is_inbound_sequence(&self, dst_chain_id: ChainID) -> bool {
		matches!(
			(self.client.get_chain_id() == dst_chain_id, self.client.if_destination_chain),
			(true, BridgeDirection::Inbound) | (false, BridgeDirection::Outbound)
		)
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self, round: U256) -> bool {
		if self.client.is_native {
			let relayer_manager = self.client.relayer_manager.as_ref().unwrap();
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
				.await
		} else if let Some((_id, native_client)) =
			self.system_clients.iter().find(|(_id, client)| client.is_native)
		{
			let relayer_manager = native_client.relayer_manager.as_ref().unwrap();
			return native_client
				.contract_call(
					relayer_manager.is_previous_selected_relayer(
						round,
						self.client.address(),
						false,
					),
					"relayer_manager.is_previous_selected_relayer",
				)
				.await
		}
		false
	}

	/// Request send bridge relay transaction to the target event channel.
	fn request_send_transaction(
		&self,
		chain_id: ChainID,
		tx_request: TransactionRequest,
		metadata: BridgeRelayMetadata,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		if let Some(event_sender) = self.event_senders.get(&chain_id) {
			match event_sender.send(EventMessage::new(
				TxRequest::Legacy(tx_request),
				EventMetadata::BridgeRelay(metadata.clone()),
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

	/// Compare the request status recorded in source chain with event status to determine if the
	/// event has already been executed
	async fn is_already_done(&self, rid: &RequestID, src_chain_id: ChainID) -> bool {
		let socket_contract =
			&self.system_clients.get(&src_chain_id).expect(INVALID_CHAIN_ID).socket;
		let request = self
			.client
			.contract_call(socket_contract.get_request(rid.clone()), "socket.get_request")
			.await;

		let event_status = request.field[0].clone();

		matches!(
			SocketEventStatus::from_u8(event_status.into()),
			SocketEventStatus::Committed | SocketEventStatus::Rollbacked
		)
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for BridgeRelayHandler<T> {
	async fn bootstrap(&self) {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping Socket events.",
			sub_display_format(SUB_LOG_TARGET),
		);

		let logs = self.get_bootstrap_events().await;

		let mut stream = tokio_stream::iter(logs);
		while let Some(log) = stream.next().await {
			self.process_confirmed_log(&log, true).await;
		}

		let mut bootstrap_count = self.bootstrapping_count.lock().await;
		*bootstrap_count += 1;

		// If All thread complete the task, starts the blockManager
		if *bootstrap_count == self.system_clients.len() as u8 {
			let mut bootstrap_guard = self.bootstrap_states.write().await;

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
		let round_info: RoundMetaData = if self.client.is_native {
			self.client
				.contract_call(self.client.authority.round_info(), "authority.round_info")
				.await
		} else if let Some((_id, native_client)) =
			self.system_clients.iter().find(|(_id, client)| client.is_native)
		{
			native_client
				.contract_call(native_client.authority.round_info(), "authority.round_info")
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
				self.bootstrap_config.round_offset,
				round_info,
			)
			.await;

		let latest_block_number = self.client.get_latest_block_number().await;
		let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
		let to_block = latest_block_number;

		let mut logs = vec![];

		// Split from_block into smaller chunks
		while from_block <= to_block {
			let chunk_to_block =
				std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

			let filter = Filter::new()
				.address(self.client.socket.address())
				.topic0(self.socket_signature)
				.from_block(from_block)
				.to_block(chunk_to_block);
			let target_logs_chunk = self.client.get_logs(&filter).await;
			logs.extend(target_logs_chunk);

			from_block = chunk_to_block + 1;
		}

		logs
	}

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_states.read().await.iter().all(|s| *s == state)
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
}
