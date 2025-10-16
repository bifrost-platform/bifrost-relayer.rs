use std::{collections::BTreeMap, sync::Arc};

use alloy::{
	network::{Network, TransactionBuilder},
	primitives::{B256, ChainId, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::{Filter, Log},
	sol_types::SolEvent,
};
use array_bytes::Hexify;
use eyre::Result;
use sc_service::SpawnTaskHandle;
use subxt::ext::subxt_core::utils::AccountId20;
use tokio::sync::{broadcast::Receiver, mpsc::UnboundedSender};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET,
		config::BOOTSTRAP_BLOCK_CHUNK_SIZE,
		errors::{INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID},
	},
	contracts::socket::{
		Socket_Struct::{Instruction, RequestID, Signatures, Socket_Message},
		SocketContract::Socket,
	},
	eth::{BootstrapState, BuiltRelayTransaction, RelayDirection, SocketEventStatus},
	substrate::{SocketMessagesSubmission, bifrost_runtime},
	tx::{
		SocketRelayMetadata, TxRequestMetadata, XtRequest, XtRequestMessage, XtRequestMetadata,
		XtRequestSender,
	},
	utils::sub_display_format,
};

use crate::{
	btc::handlers::XtRequester,
	eth::{
		ClientMap, EthClient,
		events::EventMessage,
		send_transaction,
		traits::{BootstrapHandler, Handler, SocketRelayBuilder},
	},
};

const SUB_LOG_TARGET: &str = "socket-handler";

/// The essential task that handles `socket relay` related events.
pub struct SocketRelayHandler<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<F, P, N>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The receiver that consumes new events from the block channel.
	event_stream: BroadcastStream<EventMessage>,
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	system_clients: Arc<ClientMap<F, P, N>>,
	/// The bifrost client.
	bifrost_client: Arc<EthClient<F, P, N>>,
	/// The rollback sender for each chain.
	rollback_senders: Arc<BTreeMap<ChainId, Arc<UnboundedSender<Socket_Message>>>>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

#[async_trait::async_trait]
impl<F, P, N: Network> Handler for SocketRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	async fn run(&mut self) -> Result<()> {
		let should_bootstrap = self.is_before_bootstrap_state(BootstrapState::NormalStart).await;
		if should_bootstrap {
			self.bootstrap().await?;
		}
		self.wait_for_all_chains_bootstrapped().await?;

		while let Some(Ok(msg)) = self.event_stream.next().await {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 📦 Imported #{:?} with target logs({:?})",
				sub_display_format(SUB_LOG_TARGET),
				msg.block_number,
				msg.event_logs.len(),
			);

			for log in &msg.event_logs {
				if self.is_target_contract(log) && self.is_target_event(log.topic0()) {
					self.process_confirmed_log(log, false).await?;
				}
			}
		}

		Ok(())
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) -> Result<()> {
		match log.log_decode::<Socket>() {
			Ok(decoded_log) => {
				let decoded_socket = decoded_log.inner.data;

				let msg = decoded_socket.msg.clone();
				let metadata = SocketRelayMetadata::new(
					self.is_inbound_sequence(Into::<u32>::into(msg.ins_code.ChainIndex) as ChainId),
					SocketEventStatus::from(msg.status),
					msg.req_id.sequence,
					Into::<u32>::into(msg.req_id.ChainIndex) as ChainId,
					Into::<u32>::into(msg.ins_code.ChainIndex) as ChainId,
					msg.params.to,
					is_bootstrap,
				);

				if !self.is_selected_relayer(&U256::from(msg.req_id.round_id)).await? {
					// do nothing if not selected
					return Ok(());
				}
				if self.is_sequence_ended(&msg.req_id, &msg.ins_code, metadata.status).await? {
					// do nothing if protocol sequence ended
					return Ok(());
				}
				self.submit_brp_outbound_request(msg.clone(), metadata.clone()).await?;

				self.send_socket_message(msg.clone(), metadata.clone(), metadata.is_inbound)
					.await?;
			},
			Err(error) => panic!(
				"[{}]-[{}]-[{}] Unknown error while decoding socket event: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address().await,
				error,
			),
		}

		Ok(())
	}

	#[inline]
	fn is_target_contract(&self, log: &Log) -> bool {
		match self.client.protocol_contracts.bitcoin_socket.as_ref() {
			Some(bitcoin_socket) => {
				log.address() == *self.client.protocol_contracts.socket.address()
					|| log.address() == *bitcoin_socket.address()
			},
			_ => log.address() == *self.client.protocol_contracts.socket.address(),
		}
	}

	#[inline]
	fn is_target_event(&self, topic: Option<&B256>) -> bool {
		match topic {
			Some(topic) => topic == &Socket::SIGNATURE_HASH,
			None => false,
		}
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> SocketRelayBuilder<F, P, N> for SocketRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn get_client(&self) -> Arc<EthClient<F, P, N>> {
		self.client.clone()
	}

	async fn build_transaction(
		&self,
		msg: Socket_Message,
		is_inbound: bool,
		relay_tx_chain_id: ChainId,
	) -> Result<Option<BuiltRelayTransaction<N>>> {
		if let Some(system_client) = self.system_clients.get(&relay_tx_chain_id) {
			let to_socket = system_client.protocol_contracts.socket.address();
			// the original msg must be used for building calldata
			let (signatures, is_external) = if is_inbound {
				self.build_inbound_signatures(msg.clone()).await?
			} else {
				self.build_outbound_signatures(msg.clone()).await?
			};

			return Ok(Some(BuiltRelayTransaction::new(
				self.build_poll_request(msg, signatures).with_to(*to_socket),
				is_external,
			)));
		}
		Ok(None)
	}

	async fn build_inbound_signatures(
		&self,
		mut msg: Socket_Message,
	) -> Result<(Signatures, bool)> {
		let status = SocketEventStatus::from(msg.status);
		let mut is_external = false;
		let signatures = match status {
			SocketEventStatus::Requested | SocketEventStatus::Failed => Signatures::default(),
			SocketEventStatus::Executed => {
				msg.status = SocketEventStatus::Accepted.into();
				Signatures::from(self.sign_socket_message(msg).await?)
			},
			SocketEventStatus::Reverted => {
				msg.status = SocketEventStatus::Rejected.into();
				Signatures::from(self.sign_socket_message(msg).await?)
			},
			SocketEventStatus::Accepted | SocketEventStatus::Rejected => {
				is_external = true;
				self.get_sorted_signatures(msg).await?
			},
			_ => panic!(
				"[{}]-[{}]-[{}] Unknown socket event status received: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address().await,
				status
			),
		};
		Ok((signatures, is_external))
	}

	async fn build_outbound_signatures(
		&self,
		mut msg: Socket_Message,
	) -> Result<(Signatures, bool)> {
		let status = SocketEventStatus::from(msg.status);
		let mut is_external = false;
		let signatures = match status {
			SocketEventStatus::Requested => {
				msg.status = SocketEventStatus::Accepted.into();
				Signatures::from(self.sign_socket_message(msg).await?)
			},
			SocketEventStatus::Accepted | SocketEventStatus::Rejected => {
				is_external = true;
				self.get_sorted_signatures(msg).await?
			},
			SocketEventStatus::Executed | SocketEventStatus::Reverted => Signatures::default(),
			_ => panic!(
				"[{}]-[{}]-[{}] Unknown socket event status received: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address().await,
				status
			),
		};
		Ok((signatures, is_external))
	}
}

impl<F, P, N: Network> SocketRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// Instantiates a new `SocketRelayHandler` instance.
	pub fn new(
		id: ChainId,
		event_receiver: Receiver<EventMessage>,
		system_clients: Arc<ClientMap<F, P, N>>,
		bifrost_client: Arc<EthClient<F, P, N>>,
		xt_request_sender: Arc<XtRequestSender>,
		rollback_senders: Arc<BTreeMap<ChainId, Arc<UnboundedSender<Socket_Message>>>>,
		handle: SpawnTaskHandle,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		debug_mode: bool,
	) -> Self {
		let client = system_clients.get(&id).expect(INVALID_CHAIN_ID).clone();

		Self {
			event_stream: BroadcastStream::new(event_receiver),
			client,
			system_clients,
			bifrost_client,
			xt_request_sender,
			rollback_senders,
			handle,
			bootstrap_shared_data,
			debug_mode,
		}
	}

	/// Sends the `SocketMessage` to the target chain channel.
	async fn send_socket_message(
		&self,
		socket_msg: Socket_Message,
		metadata: SocketRelayMetadata,
		is_inbound: bool,
	) -> Result<()> {
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
		if let Some(built_transaction) = self
			.build_transaction(socket_msg.clone(), is_inbound, relay_tx_chain_id)
			.await?
		{
			self.request_send_transaction(
				relay_tx_chain_id,
				built_transaction.tx_request,
				metadata,
				socket_msg,
			)
			.await;
		}

		Ok(())
	}

	/// Get the chain ID of the inbound sequence relay transaction.
	fn get_inbound_relay_tx_chain_id(
		&self,
		status: SocketEventStatus,
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
	) -> ChainId {
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
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
	) -> ChainId {
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
	) -> Result<bool> {
		let src = Into::<u32>::into(req_id.ChainIndex) as ChainId;
		let dst = Into::<u32>::into(ins_code.ChainIndex) as ChainId;

		// if inbound::accepted and relaying to bitcoin we consider as ended
		if let Some(bitcoin_chain_id) = self.client.get_bitcoin_chain_id() {
			if self.is_inbound_sequence(dst) && bitcoin_chain_id == src {
				return Ok(matches!(status, SocketEventStatus::Accepted));
			}
		}

		if let Some(src_client) = &self.system_clients.get(&src) {
			let request =
				src_client.protocol_contracts.socket.get_request(req_id.clone()).call().await?;

			return Ok(matches!(
				SocketEventStatus::from(&request.field[0]),
				SocketEventStatus::Committed | SocketEventStatus::Rollbacked
			));
		}
		Ok(false)
	}

	/// Verifies whether the socket event is an inbound sequence.
	fn is_inbound_sequence(&self, dst_chain_id: ChainId) -> bool {
		matches!(
			(self.client.chain_id() == dst_chain_id, self.client.metadata.if_destination_chain),
			(true, RelayDirection::Inbound) | (false, RelayDirection::Outbound)
		)
	}

	/// Verifies whether the socket event is on an executable(=rollbackable) state.
	fn is_executable(&self, is_inbound: bool, status: SocketEventStatus) -> bool {
		matches!(
			(is_inbound, status),
			(true, SocketEventStatus::Requested) | (false, SocketEventStatus::Accepted)
		)
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self, round: &U256) -> Result<bool> {
		let relayer_manager =
			self.bifrost_client.protocol_contracts.relayer_manager.as_ref().unwrap();
		let res = relayer_manager
			.is_previous_selected_relayer(*round, self.client.address().await, false)
			.call()
			.await?;
		Ok(res)
	}

	/// Request send socket relay transaction to the target event channel.
	async fn request_send_transaction(
		&self,
		chain_id: ChainId,
		tx_request: N::TransactionRequest,
		metadata: SocketRelayMetadata,
		socket_msg: Socket_Message,
	) {
		if let Some(client) = self.system_clients.get(&chain_id) {
			if self.is_executable(metadata.is_inbound, metadata.status) {
				self.send_rollbackable_request(chain_id, metadata.clone(), socket_msg).await;
			}

			let metadata = TxRequestMetadata::SocketRelay(metadata);
			send_transaction(
				client.clone(),
				tx_request,
				format!("{} ({})", SUB_LOG_TARGET, self.client.get_chain_name()),
				metadata,
				self.debug_mode,
				self.handle.clone(),
			);
		}
	}

	/// Sends a rollbackable socket message to the rollback channel.
	/// The received message will be handled when the interval has been reached.
	async fn send_rollbackable_request(
		&self,
		chain_id: ChainId,
		metadata: SocketRelayMetadata,
		socket_msg: Socket_Message,
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
						self.client.address().await,
						metadata,
						error
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

	async fn submit_brp_outbound_request(
		&self,
		msg: Socket_Message,
		metadata: SocketRelayMetadata,
	) -> Result<()> {
		let dst = Into::<u32>::into(msg.ins_code.ChainIndex) as ChainId;
		let status = SocketEventStatus::from(msg.status);

		if let Some(bitcoin_chain_id) = self.client.get_bitcoin_chain_id() {
			// it should be a BRP outbound request (bifrost to bitcoin)
			if self.is_inbound_sequence(dst) || dst != bitcoin_chain_id {
				return Ok(());
			}
			// we only submit the request if the status is accepted
			if status != SocketEventStatus::Accepted {
				return Ok(());
			}
			let encoded_msg = self.encode_socket_message(msg);
			let signature =
				self.client.sign_message(encoded_msg.hexify_prefixed().as_bytes()).await?.into();
			let call = XtRequest::from(bifrost_runtime::tx().blaze().submit_outbound_requests(
				SocketMessagesSubmission {
					authority_id: AccountId20(self.client.address().await.0.0),
					messages: vec![encoded_msg],
				},
				signature,
			));
			match self.xt_request_sender.send(XtRequestMessage::new(
				call,
				XtRequestMetadata::SubmitOutboundRequests(metadata.clone()),
			)) {
				Ok(_) => log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔖 Request unsigned transaction: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata
				),
				Err(error) => {
					let log_msg = format!(
						"-[{}]-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
						sub_display_format(SUB_LOG_TARGET),
						self.client.address().await,
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
		Ok(())
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> BootstrapHandler for SocketRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn get_chain_id(&self) -> ChainId {
		self.client.metadata.id
	}

	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		self.wait_for_bootstrap_state(BootstrapState::BootstrapSocketRelay).await?;

		let logs = self.get_bootstrap_events().await?;
		for log in logs {
			self.process_confirmed_log(&log, true).await?;
		}

		self.set_bootstrap_state(BootstrapState::NormalStart).await;
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] BootstrapSocketRelay → NormalStart",
			sub_display_format(SUB_LOG_TARGET),
		);
		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<Log>> {
		let mut logs = vec![];

		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			let round_info = if self.client.metadata.is_native {
				self.client.protocol_contracts.authority.round_info().call().await?
			} else {
				match self.system_clients.iter().find(|(_id, client)| client.metadata.is_native) {
					Some((_id, native_client)) => {
						native_client.protocol_contracts.authority.round_info().call().await?
					},
					_ => {
						panic!(
							"[{}]-[{}] {}",
							self.client.get_chain_name(),
							SUB_LOG_TARGET,
							INVALID_BIFROST_NATIVENESS,
						);
					},
				}
			};

			let bootstrap_offset_height = self
				.client
				.get_bootstrap_offset_height_based_on_block_time(
					bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
					round_info,
				)
				.await?;

			let latest_block_number = self.client.get_block_number().await?;
			let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
			let to_block = latest_block_number;

			// Split from_block into smaller chunks
			while from_block <= to_block {
				let chunk_to_block =
					std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

				let filter = match &self.client.protocol_contracts.bitcoin_socket {
					Some(bitcoin_socket) => Filter::new()
						.address(vec![
							*self.client.protocol_contracts.socket.address(),
							*bitcoin_socket.address(),
						])
						.event_signature(Socket::SIGNATURE_HASH)
						.from_block(from_block)
						.to_block(chunk_to_block),
					_ => Filter::new()
						.address(*self.client.protocol_contracts.socket.address())
						.event_signature(Socket::SIGNATURE_HASH)
						.from_block(from_block)
						.to_block(chunk_to_block),
				};
				let target_logs_chunk = self.client.get_logs(&filter).await?;
				logs.extend(target_logs_chunk);

				from_block = chunk_to_block + 1;
			}
		}

		Ok(logs)
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> XtRequester<F, P, N> for SocketRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<F, P, N>> {
		self.client.clone()
	}
}

#[cfg(test)]
mod tests {
	use alloy::{
		primitives::{address, bytes},
		providers::ProviderBuilder,
		sol_types::SolEvent,
	};
	use br_primitives::contracts::socket::SocketContract::{self, Socket};
	use std::sync::Arc;

	use super::*;

	#[tokio::test]
	async fn test_is_already_done() {
		let src_provider = Arc::new(ProviderBuilder::new().connect_http("".parse().unwrap()));
		let dst_provider = Arc::new(ProviderBuilder::new().connect_http("".parse().unwrap()));

		let src_socket = SocketContract::new(
			address!("d551F33Ca8eCb0Be83d8799D9C68a368BA36Dd52"),
			src_provider.clone(),
		);
		let dst_socket = SocketContract::new(
			address!("b5Fa48E8B9b89760a9f9176388D1B64A8D4968dF"),
			dst_provider.clone(),
		);

		let request_id =
			RequestID { ChainIndex: [0, 0, 11, 252].into(), round_id: 677, sequence: 3446 };
		println!("request_id : {:?}", request_id);

		let src_request = src_socket.get_request(request_id.clone()).call().await.unwrap();
		println!("src_request : {:?}", src_request);
		let dst_request = dst_socket.get_request(request_id).call().await.unwrap();
		println!("dst_request : {:?}", dst_request);
	}

	#[test]
	fn test_socket_event_decode() {
		let data = bytes!(
			"00000000000000000000000000000000000000000000000000000000000000200000bfc000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001e000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000050000271200000000000000000000000000000000000000000000000000000000030203010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e000000003000000030000bfc0148a26ea2376f006c09b7d3163f1fc70ad4134a300000000000000000000000000000000000000000000000000000000000000000000000000000000000000006e661745856b03130d03932f683cda020d7ee9ea0000000000000000000000006e661745856b03130d03932f683cda020d7ee9ea00000000000000000000000000000000000000000000000000000000000ee5e800000000000000000000000000000000000000000000000000000000000000c00000000000000000000000000000000000000000000000000000000000000000"
		);

		match Socket::abi_decode_data(&data) {
			Ok(socket) => {
				let socket_msg = socket.0;
				let req_id = socket_msg.req_id;
				let status = socket_msg.status;
				let ins_code = socket_msg.ins_code;
				let params = socket_msg.params;

				println!("req_id.chain -> {:?}", req_id.ChainIndex);
				println!("req_id.round_id -> {:?}", req_id.round_id);
				println!("req_id.sequence -> {:?}", req_id.sequence);

				println!("status -> {:?}", status);

				println!("ins_code.chain -> {:?}", ins_code.ChainIndex);
				println!("ins_code.method -> {:?}", ins_code.RBCmethod);

				println!("params.tokenIDX0 -> {:?}", params.tokenIDX0);
				println!("params.tokenIDX1 -> {:?}", params.tokenIDX1);
				println!("params.refund -> {:?}", params.refund);
				println!("params.to -> {:?}", params.to);
				println!("params.amount -> {:?}", params.amount);
				println!("params.variants -> {:?}", params.variants);
			},
			Err(error) => {
				panic!("decode failed -> {}", error);
			},
		}
	}

	#[test]
	fn test_socket_msg_decode() {
		let req_id_chain = [0, 0, 39, 18].hexify_prefixed();
		let ins_code_chain = [0, 0, 191, 192].hexify_prefixed();
		let ins_code_method = [3, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0].hexify_prefixed();

		let params_tokenidx0 = [
			0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
			0, 0, 0,
		]
		.hexify_prefixed();
		let params_tokenidx1 = [
			0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
			0, 0, 0,
		]
		.hexify_prefixed();

		println!("req_id_chain -> {:?}", req_id_chain);
		println!("ins_code_chain -> {:?}", ins_code_chain);
		println!("ins_code_method -> {:?}", ins_code_method);
		println!("params_tokenidx0 -> {:?}", params_tokenidx0);
		println!("params_tokenidx1 -> {:?}", params_tokenidx1);
	}
}
