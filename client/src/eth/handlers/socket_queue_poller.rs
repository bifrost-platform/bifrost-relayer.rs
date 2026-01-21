use std::sync::Arc;

use alloy::{
	dyn_abi::DynSolValue,
	network::Network,
	primitives::{B256, ChainId, FixedBytes, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::{Filter, Log},
	sol_types::SolEvent,
};
use array_bytes::Hexify;
use eyre::Result;
use subxt::ext::subxt_core::utils::AccountId20;
use subxt::{
	OnlineClient, backend::legacy::LegacyRpcMethods, backend::rpc::RpcClient, utils::H256,
};
use tokio::sync::broadcast::Receiver;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET, config::BOOTSTRAP_BLOCK_CHUNK_SIZE},
	contracts::socket::{Socket_Struct::Socket_Message, SocketContract::Socket},
	eth::{BootstrapState, SocketEventStatus},
	substrate::{CustomConfig, SocketMessageSubmission, bifrost_runtime},
	tx::{
		FinalizePollMetadata, OnFlightPollMetadata, XtRequestMessage, XtRequestMetadata,
		XtRequestSender,
	},
	utils::sub_display_format,
};

use crate::eth::{
	EthClient,
	events::EventMessage,
	traits::{BootstrapHandler, Handler},
};

const SUB_LOG_TARGET: &str = "socket-queue-poller";

/// The essential task that handles CCCP relay queue polling.
///
/// This handler listens for Socket events and submits on-flight poll transactions
/// to the cccp-relay-queue pallet for cross-chain transfer approval.
pub struct SocketQueuePoller<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<F, P, N>>,
	/// The Bifrost client for on-chain queries.
	pub bifrost_client: Arc<EthClient<F, P, N>>,
	/// The substrate client for storage queries.
	sub_client: OnlineClient<CustomConfig>,
	/// The legacy RPC methods for querying best block.
	sub_rpc: LegacyRpcMethods<CustomConfig>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The receiver that consumes new events from the block channel.
	event_stream: BroadcastStream<EventMessage>,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

impl<F, P, N: Network> SocketQueuePoller<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub async fn new(
		client: Arc<EthClient<F, P, N>>,
		bifrost_client: Arc<EthClient<F, P, N>>,
		sub_client: OnlineClient<CustomConfig>,
		sub_rpc_url: String,
		xt_request_sender: Arc<XtRequestSender>,
		event_receiver: Receiver<EventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Result<Self> {
		let rpc_client = RpcClient::from_url(&sub_rpc_url).await?;
		let sub_rpc = LegacyRpcMethods::<CustomConfig>::new(rpc_client);

		Ok(Self {
			client,
			bifrost_client,
			sub_client,
			sub_rpc,
			xt_request_sender,
			event_stream: BroadcastStream::new(event_receiver),
			bootstrap_shared_data,
		})
	}

	/// Check if this is an inbound transfer (external chain -> Bifrost).
	fn is_inbound(&self, dst_chain_id: ChainId) -> bool {
		dst_chain_id == self.bifrost_client.chain_id()
	}

	/// Extract asset_index_hash from socket message params.
	fn get_asset_index_hash(&self, msg: &Socket_Message) -> B256 {
		msg.params.tokenIDX0
	}

	/// Extract sequence ID from socket message.
	fn get_sequence_id(&self, msg: &Socket_Message) -> u128 {
		msg.req_id.sequence
	}

	/// Extract source chain ID from socket message.
	fn get_src_chain_id(&self, msg: &Socket_Message) -> ChainId {
		Into::<u32>::into(msg.req_id.ChainIndex) as ChainId
	}

	/// Extract destination chain ID from socket message.
	fn get_dst_chain_id(&self, msg: &Socket_Message) -> ChainId {
		Into::<u32>::into(msg.ins_code.ChainIndex) as ChainId
	}

	/// Query OnFlightTransfers storage to check if transfer already exists.
	///
	/// Returns `true` if transfer exists, `false` otherwise.
	/// Storage key: (src_chain_id, src_tx_id)
	async fn is_on_flight_transfer_exists(
		&self,
		src_chain_id: ChainId,
		src_tx_id: B256,
	) -> Result<bool> {
		// Query at best block (including unfinalized) instead of latest finalized
		let best_hash = self.sub_rpc.chain_get_block_hash(None).await?.unwrap_or_default();
		let storage = self.sub_client.storage().at(best_hash);

		let transfer = storage
			.fetch(
				&bifrost_runtime::storage()
					.cccp_relay_queue()
					.on_flight_transfers(src_chain_id as u32, H256(src_tx_id.0)),
			)
			.await?;

		Ok(transfer.is_some())
	}

	/// Process a confirmed Socket event log.
	async fn process_confirmed_log(&self, log: &Log, _is_bootstrap: bool) -> Result<()> {
		match log.log_decode::<Socket>() {
			Ok(decoded_log) => {
				let msg = decoded_log.inner.data.msg.clone();
				let status = SocketEventStatus::from(msg.status);

				let asset_index_hash = self.get_asset_index_hash(&msg);
				let sequence_id = self.get_sequence_id(&msg);
				let src_chain_id = self.get_src_chain_id(&msg);
				let dst_chain_id = self.get_dst_chain_id(&msg);
				let is_inbound = self.is_inbound(dst_chain_id);
				let src_tx_id = log.transaction_hash.unwrap_or_default();

				match status {
					SocketEventStatus::Requested => {
						self.process_requested(
							msg,
							is_inbound,
							sequence_id,
							src_chain_id,
							dst_chain_id,
							asset_index_hash,
							src_tx_id,
						)
						.await?;
					},
					SocketEventStatus::Committed | SocketEventStatus::Rollbacked => {
						self.process_finalized(
							msg,
							is_inbound,
							sequence_id,
							src_chain_id,
							dst_chain_id,
							status,
							src_tx_id,
						)
						.await?;
					},
					_ => {
						// Ignore other statuses
					},
				}
			},
			Err(error) => {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address().await,
					"❗️ Failed to decode Socket event: {:?}",
					error
				);
			},
		}

		Ok(())
	}

	/// Process REQUESTED status - submit on_flight_poll.
	async fn process_requested(
		&self,
		msg: Socket_Message,
		is_inbound: bool,
		sequence_id: u128,
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
		asset_index_hash: B256,
		src_tx_id: B256,
	) -> Result<()> {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 📨 Socket event detected: {} seq={}, asset={}, {} -> {}",
			sub_display_format(SUB_LOG_TARGET),
			if is_inbound { "Inbound" } else { "Outbound" },
			sequence_id,
			asset_index_hash,
			src_chain_id,
			dst_chain_id,
		);

		// Query OnFlightTransfers storage to check current state
		// Storage key: (src_chain_id, src_tx_id)
		let transfer_exists = self.is_on_flight_transfer_exists(src_chain_id, src_tx_id).await?;

		if transfer_exists {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] Transfer already in OnFlightTransfers, skipping: seq={}",
				sub_display_format(SUB_LOG_TARGET),
				sequence_id,
			);
			return Ok(());
		}

		let metadata =
			OnFlightPollMetadata::new(is_inbound, sequence_id, src_chain_id, dst_chain_id);

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 🚀 Submitting on-flight poll: {}",
			sub_display_format(SUB_LOG_TARGET),
			metadata,
		);

		self.submit_on_flight_poll(msg, metadata, src_tx_id).await
	}

	/// Process COMMITTED/ROLLBACKED status - submit finalize_poll.
	async fn process_finalized(
		&self,
		msg: Socket_Message,
		is_inbound: bool,
		sequence_id: u128,
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
		status: SocketEventStatus,
		src_tx_id: B256,
	) -> Result<()> {
		let is_committed = status == SocketEventStatus::Committed;

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 📨 Socket {} event detected: {} seq={}, {} -> {}",
			sub_display_format(SUB_LOG_TARGET),
			if is_committed { "Committed" } else { "Rollbacked" },
			if is_inbound { "Inbound" } else { "Outbound" },
			sequence_id,
			src_chain_id,
			dst_chain_id,
		);

		let metadata = FinalizePollMetadata::new(
			is_inbound,
			sequence_id,
			src_chain_id,
			dst_chain_id,
			is_committed,
		);

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 🏁 Submitting finalize poll: {}",
			sub_display_format(SUB_LOG_TARGET),
			metadata,
		);

		self.submit_finalize_poll(msg, metadata, src_tx_id).await
	}

	/// Check if the log is from the target Socket contract.
	#[inline]
	fn is_target_contract(&self, log: &Log) -> bool {
		log.address() == *self.client.protocol_contracts.socket.address()
	}

	/// Check if the log topic matches Socket event signature.
	#[inline]
	fn is_target_event(&self, topic: Option<&B256>) -> bool {
		match topic {
			Some(topic) => topic == &Socket::SIGNATURE_HASH,
			None => false,
		}
	}

	/// Encode a socket message for signing.
	fn encode_socket_message(&self, msg: &Socket_Message) -> Vec<u8> {
		let req_id = DynSolValue::Tuple(vec![
			DynSolValue::FixedBytes(
				FixedBytes::<32>::right_padding_from(msg.req_id.ChainIndex.as_slice()),
				4,
			),
			DynSolValue::Uint(U256::from(msg.req_id.round_id), 64),
			DynSolValue::Uint(U256::from(msg.req_id.sequence), 128),
		]);
		let status = DynSolValue::Uint(U256::from(msg.status), 8);
		let ins_code = DynSolValue::Tuple(vec![
			DynSolValue::FixedBytes(
				FixedBytes::<32>::right_padding_from(msg.ins_code.ChainIndex.as_slice()),
				4,
			),
			DynSolValue::FixedBytes(
				FixedBytes::<32>::right_padding_from(msg.ins_code.RBCmethod.as_slice()),
				16,
			),
		]);
		let params = DynSolValue::Tuple(vec![
			DynSolValue::FixedBytes(msg.params.tokenIDX0, 32),
			DynSolValue::FixedBytes(msg.params.tokenIDX1, 32),
			DynSolValue::Address(msg.params.refund),
			DynSolValue::Address(msg.params.to),
			DynSolValue::Uint(msg.params.amount, 256),
			DynSolValue::Bytes(msg.params.variants.to_vec()),
		]);

		DynSolValue::Tuple(vec![req_id, status, ins_code, params]).abi_encode()
	}

	/// Submit on_flight_poll extrinsic to the cccp-relay-queue pallet.
	async fn submit_on_flight_poll(
		&self,
		msg: Socket_Message,
		metadata: OnFlightPollMetadata,
		src_tx_id: B256,
	) -> Result<()> {
		let encoded_msg = self.encode_socket_message(&msg);
		let signature =
			self.client.sign_message(encoded_msg.hexify_prefixed().as_bytes()).await?.into();

		let call = Arc::new(bifrost_runtime::tx().cccp_relay_queue().on_flight_poll(
			SocketMessageSubmission {
				authority_id: AccountId20(self.client.address().await.0.0),
				message: encoded_msg.into(),
				src_tx_id: H256(src_tx_id.0),
			},
			signature,
		));

		match self
			.xt_request_sender
			.send(XtRequestMessage::new(call, XtRequestMetadata::OnFlightPoll(metadata.clone())))
		{
			Ok(_) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address().await,
					"❗️ Failed to send unsigned transaction: {}, Error: {}",
					metadata,
					error
				);
			},
		}

		Ok(())
	}

	/// Submit finalize_poll extrinsic to the cccp-relay-queue pallet.
	async fn submit_finalize_poll(
		&self,
		msg: Socket_Message,
		metadata: FinalizePollMetadata,
		src_tx_id: B256,
	) -> Result<()> {
		let encoded_msg = self.encode_socket_message(&msg);
		let signature =
			self.client.sign_message(encoded_msg.hexify_prefixed().as_bytes()).await?.into();

		let call = Arc::new(bifrost_runtime::tx().cccp_relay_queue().finalize_poll(
			SocketMessageSubmission {
				authority_id: AccountId20(self.client.address().await.0.0),
				message: encoded_msg.into(),
				src_tx_id: H256(src_tx_id.0),
			},
			signature,
		));

		match self
			.xt_request_sender
			.send(XtRequestMessage::new(call, XtRequestMetadata::FinalizePoll(metadata.clone())))
		{
			Ok(_) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address().await,
					"❗️ Failed to send unsigned transaction: {}, Error: {}",
					metadata,
					error
				);
			},
		}

		Ok(())
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> Handler for SocketQueuePoller<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	async fn run(&mut self) -> Result<()> {
		// Run bootstrap to catch up on historical Requested/Committed/Rollbacked events
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
		self.process_confirmed_log(log, is_bootstrap).await
	}

	#[inline]
	fn is_target_contract(&self, log: &Log) -> bool {
		self.is_target_contract(log)
	}

	#[inline]
	fn is_target_event(&self, topic: Option<&B256>) -> bool {
		self.is_target_event(topic)
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> BootstrapHandler for SocketQueuePoller<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn get_chain_id(&self) -> ChainId {
		self.client.chain_id()
	}

	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		// Wait for BootstrapSocketRelayQueue phase
		self.wait_for_bootstrap_state(BootstrapState::BootstrapSocketRelayQueue).await?;

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Starting SocketQueuePoller bootstrap...",
			sub_display_format(SUB_LOG_TARGET),
		);

		let logs = self.get_bootstrap_events().await?;
		let total_logs = logs.len();

		for log in logs {
			if let Err(e) = self.process_confirmed_log(&log, true).await {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address().await,
					"❗️ Error processing bootstrap log: {:?}",
					e
				);
			}
		}

		// Transition to BootstrapSocketRelay for SocketRelayHandler
		self.set_bootstrap_state(BootstrapState::BootstrapSocketRelay).await;
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] BootstrapSocketRelayQueue → BootstrapSocketRelay (processed {} events)",
			sub_display_format(SUB_LOG_TARGET),
			total_logs,
		);

		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<Log>> {
		let mut logs = vec![];

		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			// Get round info from Bifrost chain for calculating bootstrap offset
			let round_info = if self.client.metadata.is_native {
				self.client.protocol_contracts.authority.round_info().call().await?
			} else {
				// Bifrost client should always have authority contract
				self.bifrost_client.protocol_contracts.authority.round_info().call().await?
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

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚙️  [Bootstrap mode] Fetching Socket events from block {} to {}",
				sub_display_format(SUB_LOG_TARGET),
				from_block,
				to_block,
			);

			// Split into smaller chunks to avoid RPC limits
			while from_block <= to_block {
				let chunk_to_block =
					std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

				let filter = Filter::new()
					.address(*self.client.protocol_contracts.socket.address())
					.event_signature(Socket::SIGNATURE_HASH)
					.from_block(from_block)
					.to_block(chunk_to_block);

				let target_logs_chunk = self.client.get_logs(&filter).await?;
				logs.extend(target_logs_chunk);

				from_block = chunk_to_block + 1;
			}

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚙️  [Bootstrap mode] Found {} Socket events for queue poller",
				sub_display_format(SUB_LOG_TARGET),
				logs.len(),
			);
		}

		Ok(logs)
	}
}
