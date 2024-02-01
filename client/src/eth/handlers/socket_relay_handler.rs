#[cfg(feature = "v2")]
use {
	br_primitives::eth::SocketVariants,
	ethers::{
		abi::{ParamType, Token},
		types::Bytes,
	},
	std::str::FromStr,
};

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use ethers::{
	abi::RawLog,
	prelude::decode_logs,
	providers::JsonRpcClient,
	types::{Filter, Log, TransactionRequest, H256, U256},
};
use sc_service::SpawnTaskHandle;
use tokio::{sync::broadcast::Receiver, time::sleep};
use tokio_stream::StreamExt;

use br_primitives::{
	authority::RoundMetaData,
	bootstrap::{BootstrapHandler, BootstrapSharedData},
	constants::DEFAULT_BOOTSTRAP_ROUND_OFFSET,
	eth::{
		BootstrapState, BuiltRelayTransaction, ChainID, GasCoefficient, RelayDirection,
		SocketEventStatus, SocketVersion, BOOTSTRAP_BLOCK_CHUNK_SIZE,
	},
	socket::{RequestID, Signatures, SocketEvents, SocketMessage},
	sub_display_format, RollbackSender, INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID,
	INVALID_CONTRACT_ABI,
};

use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler,
	SocketRelayMetadata, TxRequest,
};

use super::{ExecutionFilter, SocketRelayBuilder};

const SUB_LOG_TARGET: &str = "socket-handler";

/// The essential task that handles `socket relay` related events.
pub struct SocketRelayHandler<T> {
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The event senders that sends messages to the event channel.
	event_senders: BTreeMap<ChainID, Arc<EventSender>>,
	/// The rollback senders that sends rollbackable socket messages.
	rollback_senders: BTreeMap<ChainID, Arc<RollbackSender>>,
	/// The block receiver that consumes new blocks from the block channel.
	block_receiver: Receiver<BlockMessage>,
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	system_clients: BTreeMap<ChainID, Arc<EthClient<T>>>,
	/// Signature of the `Socket` Event.
	socket_signature: H256,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// A handle for spawning execution filter tasks.
	execute_spawn_handle: SpawnTaskHandle,
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> Handler for SocketRelayHandler<T> {
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
					#[allow(unused_mut)]
					let mut metadata = SocketRelayMetadata::new(
						self.is_inbound_sequence(ChainID::from_be_bytes(msg.ins_code.chain)),
						SocketEventStatus::from_u8(msg.status),
						msg.req_id.sequence,
						ChainID::from_be_bytes(msg.req_id.chain),
						ChainID::from_be_bytes(msg.ins_code.chain),
						msg.params.to,
						is_bootstrap,
					);

					#[cfg(feature = "v2")]
					{
						metadata.variants = self.decode_msg_variants(&msg.params.variants);
					}

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
					if self.is_sequence_ended(&msg.req_id, metadata.src_chain_id).await {
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

	#[cfg(feature = "v2")]
	fn decode_msg_variants(&self, raw_variants: &Bytes) -> SocketVariants {
		if raw_variants != &Bytes::default() && raw_variants != &Bytes::from_str("0x00").unwrap() {
			match ethers::abi::decode(
				&[
					ParamType::FixedBytes(4),
					ParamType::Address,
					ParamType::Uint(256),
					ParamType::Bytes,
				],
				raw_variants,
			) {
				Ok(variants) => {
					let mut result = SocketVariants::default();
					variants.into_iter().for_each(|variant| match variant {
						Token::FixedBytes(source_chain) => {
							result.source_chain = Bytes::from(source_chain);
						},
						Token::Address(receiver) => {
							result.receiver = receiver;
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
					result.version = SocketVersion::V2;
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
		SocketVariants::default()
	}
}

impl<T: 'static + JsonRpcClient> SocketRelayHandler<T> {
	/// Instantiates a new `SocketRelayHandler` instance.
	pub fn new(
		id: ChainID,
		event_senders_vec: Vec<Arc<EventSender>>,
		rollback_senders: BTreeMap<ChainID, Arc<RollbackSender>>,
		block_receiver: Receiver<BlockMessage>,
		system_clients_vec: Vec<Arc<EthClient<T>>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		execute_spawn_handle: SpawnTaskHandle,
	) -> Self {
		let system_clients: BTreeMap<ChainID, Arc<EthClient<T>>> = system_clients_vec
			.iter()
			.map(|client| (client.get_chain_id(), client.clone()))
			.collect();

		let client = system_clients.get(&id).expect(INVALID_CHAIN_ID).clone();

		let event_senders: BTreeMap<ChainID, Arc<EventSender>> = event_senders_vec
			.iter()
			.map(|event_sender| (event_sender.id, event_sender.clone()))
			.collect();

		Self {
			event_senders,
			rollback_senders,
			block_receiver,
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
			execute_spawn_handle,
		}
	}

	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(
		&self,
		socket_msg: SocketMessage,
		metadata: SocketRelayMetadata,
		is_inbound: bool,
	) {
		let status = SocketEventStatus::from_u8(socket_msg.status);

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
		if let Some(event_sender) = self.event_senders.get(&chain_id) {
			if self.is_executable(metadata.is_inbound, metadata.status) {
				self.send_rollbackable_request(chain_id, metadata.clone(), socket_msg);

				match metadata.variants.version {
					SocketVersion::V1 => {
						// nothing to do on version 1.
					},
					SocketVersion::V2 => {
						let task = ExecutionFilter::new(
							self.get_client(),
							event_sender.clone(),
							metadata.clone(),
						);
						self.execute_spawn_handle.spawn("execution_filter", None, async move {
							task.try_filter_and_send(
								tx_request,
								give_random_delay,
								GasCoefficient::Mid,
							)
							.await
						});
						return;
					},
				}
			}

			match event_sender.send(EventMessage::new(
				TxRequest::Legacy(tx_request),
				EventMetadata::SocketRelay(metadata.clone()),
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
					log::error!(
						target: &self.client.get_chain_name(),
						"-[{}] ❗️ Failed to store rollbackable socket message: {}, Error: {}",
						sub_display_format(SUB_LOG_TARGET),
						metadata,
						error.to_string()
					);
					sentry::capture_message(
						format!(
							"[{}]-[{}]-[{}] ❗️ Failed to store rollbackable socket message: {}, Error: {}",
							&self.client.get_chain_name(),
							SUB_LOG_TARGET,
							self.client.address(),
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
impl<T: 'static + JsonRpcClient> BootstrapHandler for SocketRelayHandler<T> {
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
		abi::ParamType,
		providers::{Http, Provider},
		types::{Bytes, H160},
	};

	use br_primitives::socket::SocketContract;

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
		let topic =
			H256::from_str("0x918454f530580823dd0d8cf59cacb45a6eb7cc62f222d7129efba5821e77f191")
				.unwrap();
		let data = Bytes::from_str("0x000000000000000000000000000000000000000000000000000000000000002000000bfc0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002a50000000000000000000000000000000000000000000000000000000000000d7600000000000000000000000000000000000000000000000000000000000000010000003800000000000000000000000000000000000000000000000000000000030203010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e00000000a0000000300000bfcb6786b5c260c1f98391e3f9230d757a38639fa570000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff7116560005a341f809bb6405260bbb0f2a1b45000000000000000000000000ff7116560005a341f809bb6405260bbb0f2a1b4500000000000000000000000000000000000000000000000005938a6ebdb6900000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000").unwrap();

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
			Ok(socket) => {
				println!("socket -> {:?}", socket);
				println!("socket -> {:?}", socket[0].to_string());
			},
			Err(error) => {
				panic!("decode failed -> {}", error);
			},
		}
	}

	#[cfg(feature = "v2")]
	#[test]
	fn test_socket_variants_decode() {
		println!("default byte -> {}", Bytes::default());
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
					Token::Address(receiver) => {
						result.receiver = receiver;
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
