use std::{collections::BTreeMap, sync::Arc, time::Duration};

use ethers::{
	abi::RawLog,
	prelude::decode_logs,
	providers::JsonRpcClient,
	types::{Filter, Log, TransactionRequest, H256, U256},
};
use tokio::{sync::broadcast::Receiver, time::sleep};
use tokio_stream::StreamExt;

use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET,
		config::BOOTSTRAP_BLOCK_CHUNK_SIZE,
		errors::{INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_CONTRACT_ABI},
	},
	contracts::{
		authority::RoundMetaData,
		socket::{Instruction, RequestID, Signatures, SocketEvents, SocketMessage},
	},
	eth::{
		BootstrapState, BuiltRelayTransaction, ChainID, GasCoefficient, RelayDirection,
		SocketEventStatus,
	},
	periodic::RollbackSender,
	tx::{SocketRelayMetadata, TxRequest, TxRequestMessage, TxRequestMetadata, TxRequestSender},
	utils::sub_display_format,
};

use crate::eth::{
	events::EventMessage,
	traits::{BootstrapHandler, Handler, SocketRelayBuilder},
	EthClient,
};

const SUB_LOG_TARGET: &str = "socket-handler";

/// The essential task that handles `socket relay` related events.
pub struct SocketRelayHandler<T> {
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The senders that sends messages to the tx request channel.
	tx_request_senders: BTreeMap<ChainID, Arc<TxRequestSender>>,
	/// The rollback senders that sends rollbackable socket messages.
	rollback_senders: BTreeMap<ChainID, Arc<RollbackSender>>,
	/// The receiver that consumes new events from the block channel.
	event_receiver: Receiver<EventMessage>,
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	system_clients: BTreeMap<ChainID, Arc<EthClient<T>>>,
	/// Signature of the `Socket` Event.
	socket_signature: H256,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> Handler for SocketRelayHandler<T> {
	async fn run(&mut self) {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapSocketRelay).await {
				self.bootstrap().await;

				sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let msg = self.event_receiver.recv().await.unwrap();

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 📦 Imported #{:?} with target logs({:?})",
					sub_display_format(SUB_LOG_TARGET),
					msg.block_number,
					msg.event_logs.len(),
				);

				let mut stream = tokio_stream::iter(msg.event_logs);
				while let Some(log) = stream.next().await {
					if self.is_target_contract(&log) && self.is_target_event(log.topics[0]) {
						self.process_confirmed_log(&log, false).await;
					}
				}
			}
		}
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) {
		match decode_logs::<SocketEvents>(&[RawLog::from(log.clone())]) {
			Ok(decoded) => match &decoded[0] {
				SocketEvents::Socket(socket) => {
					let msg = socket.clone().msg;
					let metadata = SocketRelayMetadata::new(
						self.is_inbound_sequence(ChainID::from_be_bytes(msg.ins_code.chain)),
						SocketEventStatus::from(msg.status),
						msg.req_id.sequence,
						ChainID::from_be_bytes(msg.req_id.chain),
						ChainID::from_be_bytes(msg.ins_code.chain),
						msg.params.to,
						is_bootstrap,
					);

					if !metadata.is_bootstrap {
						log::info!(
							target: &self.client.get_chain_name(),
							"-[{}] 🔖 Detected socket event: {}, {:?}-{:?}",
							sub_display_format(SUB_LOG_TARGET),
							metadata,
							log.block_number.unwrap(),
							log.transaction_hash.unwrap(),
						);
					}

					if !self.is_selected_relayer(&msg.req_id.round_id.into()).await {
						// do nothing if not selected
						return;
					}
					if self.is_sequence_ended(&msg.req_id, &msg.ins_code, metadata.status).await {
						// do nothing if protocol sequence ended
						return;
					}

					self.send_socket_message(msg.clone(), metadata.clone(), metadata.is_inbound)
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

	#[inline]
	fn is_target_contract(&self, log: &Log) -> bool {
		if let Some(bitcoin_socket) = self.client.protocol_contracts.bitcoin_socket.as_ref() {
			log.address == self.client.protocol_contracts.socket.address()
				|| log.address == bitcoin_socket.address()
		} else {
			log.address == self.client.protocol_contracts.socket.address()
		}
	}

	#[inline]
	fn is_target_event(&self, topic: H256) -> bool {
		topic == self.socket_signature
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> SocketRelayBuilder<T> for SocketRelayHandler<T> {
	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}

	async fn build_transaction(
		&self,
		msg: SocketMessage,
		is_inbound: bool,
		relay_tx_chain_id: ChainID,
	) -> Option<BuiltRelayTransaction> {
		if let Some(system_client) = self.system_clients.get(&relay_tx_chain_id) {
			let to_socket = system_client.protocol_contracts.socket.address();
			// the original msg must be used for building calldata
			let (signatures, is_external) = if is_inbound {
				self.build_inbound_signatures(msg.clone()).await
			} else {
				self.build_outbound_signatures(msg.clone()).await
			};
			return Some(BuiltRelayTransaction::new(
				TransactionRequest::default()
					.data(self.build_poll_call_data(msg, signatures))
					.to(to_socket),
				is_external,
			));
		}
		None
	}

	async fn build_inbound_signatures(&self, mut msg: SocketMessage) -> (Signatures, bool) {
		let status = SocketEventStatus::from(msg.status);
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
		let status = SocketEventStatus::from(msg.status);
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
}

impl<T: 'static + JsonRpcClient> SocketRelayHandler<T> {
	/// Instantiates a new `SocketRelayHandler` instance.
	pub fn new(
		id: ChainID,
		tx_request_senders_vec: Vec<Arc<TxRequestSender>>,
		rollback_senders: BTreeMap<ChainID, Arc<RollbackSender>>,
		event_receiver: Receiver<EventMessage>,
		system_clients_vec: Vec<Arc<EthClient<T>>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Self {
		let system_clients: BTreeMap<ChainID, Arc<EthClient<T>>> = system_clients_vec
			.iter()
			.map(|client| (client.get_chain_id(), client.clone()))
			.collect();

		let client = system_clients.get(&id).expect(INVALID_CHAIN_ID).clone();

		let tx_request_senders: BTreeMap<ChainID, Arc<TxRequestSender>> = tx_request_senders_vec
			.iter()
			.map(|sender| (sender.id, sender.clone()))
			.collect();

		Self {
			tx_request_senders,
			rollback_senders,
			event_receiver,
			socket_signature: client
				.protocol_contracts
				.socket
				.abi()
				.event("Socket")
				.expect(INVALID_CONTRACT_ABI)
				.signature(),
			client,
			system_clients,
			bootstrap_shared_data,
		}
	}

	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(
		&self,
		socket_msg: SocketMessage,
		metadata: SocketRelayMetadata,
		is_inbound: bool,
	) {
		let status = SocketEventStatus::from(socket_msg.status);

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
			self.build_transaction(socket_msg.clone(), is_inbound, relay_tx_chain_id).await
		{
			self.request_send_transaction(
				relay_tx_chain_id,
				built_transaction.tx_request,
				metadata,
				socket_msg,
				built_transaction.is_external,
				if built_transaction.is_external {
					GasCoefficient::Low
				} else {
					GasCoefficient::Mid
				},
			)
			.await;
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
	async fn is_sequence_ended(
		&self,
		req_id: &RequestID,
		ins_code: &Instruction,
		status: SocketEventStatus,
	) -> bool {
		let src = ChainID::from_be_bytes(req_id.chain);
		let dst = ChainID::from_be_bytes(ins_code.chain);

		// if inbound::accepted and relaying to bitcoin we consider as ended
		if let Some(bitcoin_chain_id) = self.client.get_bitcoin_chain_id() {
			if self.is_inbound_sequence(dst) && bitcoin_chain_id == src {
				return matches!(status, SocketEventStatus::Accepted);
			}
		}

		if let Some(src_client) = &self.system_clients.get(&src) {
			let request = src_client
				.contract_call(
					src_client.protocol_contracts.socket.get_request(req_id.clone()),
					"socket.get_request",
				)
				.await;

			return matches!(
				SocketEventStatus::from(&request.field[0]),
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

	/// Verifies whether the socket event is on a executable(=rollbackable) state.
	fn is_executable(&self, is_inbound: bool, status: SocketEventStatus) -> bool {
		matches!(
			(is_inbound, status),
			(true, SocketEventStatus::Requested) | (false, SocketEventStatus::Accepted)
		)
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self, round: &U256) -> bool {
		if self.client.metadata.is_native {
			let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
			return self
				.client
				.contract_call(
					relayer_manager.is_previous_selected_relayer(
						*round,
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
						*round,
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
	async fn request_send_transaction(
		&self,
		chain_id: ChainID,
		tx_request: TransactionRequest,
		metadata: SocketRelayMetadata,
		socket_msg: SocketMessage,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		if let Some(sender) = self.tx_request_senders.get(&chain_id) {
			if self.is_executable(metadata.is_inbound, metadata.status) {
				self.send_rollbackable_request(chain_id, metadata.clone(), socket_msg);
			}

			match sender.send(TxRequestMessage::new(
				TxRequest::Legacy(tx_request),
				TxRequestMetadata::SocketRelay(metadata.clone()),
				true,
				give_random_delay,
				gas_coefficient,
				metadata.is_bootstrap,
			)) {
				Ok(()) => log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔖 Request relay transaction to chain({:?}): {}",
					sub_display_format(SUB_LOG_TARGET),
					chain_id,
					metadata
				),
				Err(error) => {
					let log_msg = format!(
						"-[{}]-[{}] ❗️ Failed to send relay transaction to chain({:?}): {}, Error: {}",
						sub_display_format(SUB_LOG_TARGET),
						self.client.address(),
						chain_id,
						metadata,
						error
					);
					log::error!(target: &self.client.get_chain_name(), "{log_msg}");
					sentry::capture_message(
						&format!("[{}]{log_msg}", &self.client.get_chain_name()),
						sentry::Level::Error,
					);
				},
			}
		}
	}

	/// Sends a rollbackable socket message to the rollback channel.
	/// The received message will be handled when the interval has been reached.
	fn send_rollbackable_request(
		&self,
		chain_id: ChainID,
		metadata: SocketRelayMetadata,
		socket_msg: SocketMessage,
	) {
		if let Some(rollback_sender) = self.rollback_senders.get(&chain_id) {
			match rollback_sender.send(socket_msg) {
				Ok(()) => log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔃 Store rollbackable socket message: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata
				),
				Err(error) => {
					let msg = format!(
						"-[{}]-[{}] ❗️ Failed to store rollbackable socket message: {}, Error: {}",
						sub_display_format(SUB_LOG_TARGET),
						self.client.address(),
						metadata,
						error.to_string()
					);
					log::error!(target: &self.client.get_chain_name(), "{msg}");
					sentry::capture_message(
						&format!("[{}]{msg}", &self.client.get_chain_name()),
						sentry::Level::Error,
					);
				},
			}
		}
	}
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> BootstrapHandler for SocketRelayHandler<T> {
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

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

		let mut bootstrap_count = self.bootstrap_shared_data.socket_bootstrap_count.lock().await;
		*bootstrap_count += 1;

		// If All thread complete the task, starts the blockManager
		if *bootstrap_count == self.bootstrap_shared_data.system_providers_len as u8 {
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

				let filter =
					if let Some(bitcoin_socket) = &self.client.protocol_contracts.bitcoin_socket {
						Filter::new()
							.address(vec![
								self.client.protocol_contracts.socket.address(),
								bitcoin_socket.address(),
							])
							.topic0(self.socket_signature)
							.from_block(from_block)
							.to_block(chunk_to_block)
					} else {
						Filter::new()
							.address(self.client.protocol_contracts.socket.address())
							.topic0(self.socket_signature)
							.from_block(from_block)
							.to_block(chunk_to_block)
					};
				let target_logs_chunk = self.client.get_logs(&filter).await;
				logs.extend(target_logs_chunk);

				from_block = chunk_to_block + 1;
			}
		}

		logs
	}
}

#[cfg(test)]
mod tests {
	use std::{str::FromStr, sync::Arc};

	use ethers::{
		abi::AbiDecode,
		providers::{Http, Provider},
		types::{Bytes, H160},
		utils::hex::ToHexExt,
	};

	use br_primitives::contracts::socket::{Socket, SocketContract};

	use super::*;

	#[tokio::test]
	async fn test_is_already_done() {
		let src_provider = Arc::new(Provider::<Http>::try_from("").unwrap());
		let dst_provider = Arc::new(Provider::<Http>::try_from("").unwrap());

		let src_socket = SocketContract::new(
			H160::from_str("0xd551F33Ca8eCb0Be83d8799D9C68a368BA36Dd52").unwrap(),
			src_provider.clone(),
		);
		let dst_socket = SocketContract::new(
			H160::from_str("0xb5Fa48E8B9b89760a9f9176388D1B64A8D4968dF").unwrap(),
			dst_provider.clone(),
		);

		let request_id: RequestID =
			RequestID { chain: [0, 0, 11, 252], round_id: 677, sequence: 3446 };
		println!("request_id : {:?}", request_id);

		let src_request = src_socket.get_request(request_id.clone()).call().await.unwrap();
		println!("src_request : {:?}", src_request);
		let dst_request = dst_socket.get_request(request_id).call().await.unwrap();
		println!("dst_request : {:?}", dst_request);
	}

	#[test]
	fn test_socket_event_decode() {
		let data = Bytes::from_str("0x00000000000000000000000000000000000000000000000000000000000000200000bfc000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001e000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000050000271200000000000000000000000000000000000000000000000000000000030203010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e000000003000000030000bfc0148a26ea2376f006c09b7d3163f1fc70ad4134a300000000000000000000000000000000000000000000000000000000000000000000000000000000000000006e661745856b03130d03932f683cda020d7ee9ea0000000000000000000000006e661745856b03130d03932f683cda020d7ee9ea00000000000000000000000000000000000000000000000000000000000ee5e800000000000000000000000000000000000000000000000000000000000000c00000000000000000000000000000000000000000000000000000000000000000").unwrap();

		match Socket::decode(&data) {
			Ok(socket) => {
				let req_id = socket.msg.req_id;
				let status = socket.msg.status;
				let ins_code = socket.msg.ins_code;
				let params = socket.msg.params;

				println!("req_id.chain -> {:?}", req_id.chain.encode_hex_with_prefix());
				println!("req_id.round_id -> {:?}", req_id.round_id);
				println!("req_id.sequence -> {:?}", req_id.sequence);

				println!("status -> {:?}", status);

				println!("ins_code.chain -> {:?}", ins_code.chain.encode_hex_with_prefix());
				println!("ins_code.method -> {:?}", ins_code.method.encode_hex_with_prefix());

				println!("params.tokenIDX0 -> {:?}", params.token_idx0.encode_hex_with_prefix());
				println!("params.tokenIDX1 -> {:?}", params.token_idx1.encode_hex_with_prefix());
				println!("params.refund -> {:?}", params.refund);
				println!("params.to -> {:?}", params.to);
				println!("params.amount -> {:?}", params.amount);
				println!("params.variants -> {:?}", params.variants.to_string());
			},
			Err(error) => {
				panic!("decode failed -> {}", error);
			},
		}
	}

	#[test]
	fn test_socket_msg_decode() {
		let req_id_chain = array_bytes::bytes2hex("0x", [0, 0, 39, 18]);
		let ins_code_chain = array_bytes::bytes2hex("0x", [0, 0, 191, 192]);
		let ins_code_method =
			array_bytes::bytes2hex("0x", [3, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

		let params_tokenidx0 = array_bytes::bytes2hex(
			"0x",
			[
				0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
				0, 0, 0, 0,
			],
		);
		let params_tokenidx1 = array_bytes::bytes2hex(
			"0x",
			[
				0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
				0, 0, 0, 0,
			],
		);

		println!("req_id_chain -> {:?}", req_id_chain);
		println!("ins_code_chain -> {:?}", ins_code_chain);
		println!("ins_code_method -> {:?}", ins_code_method);
		println!("params_tokenidx0 -> {:?}", params_tokenidx0);
		println!("params_tokenidx1 -> {:?}", params_tokenidx1);
	}
}
