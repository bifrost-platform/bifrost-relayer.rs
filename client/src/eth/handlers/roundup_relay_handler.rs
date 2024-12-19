use std::{sync::Arc, time::Duration};

use alloy::{
	network::{primitives::ReceiptResponse as _, AnyNetwork},
	primitives::{Address, PrimitiveSignature, B256, U256},
	providers::{fillers::TxFiller, Provider, WalletProvider},
	rpc::types::{Filter, Log, TransactionInput, TransactionRequest},
	sol_types::SolEvent as _,
	transports::Transport,
};
use async_trait::async_trait;
use eyre::Result;
use sc_service::SpawnTaskHandle;
use tokio::{sync::broadcast::Receiver, time::sleep};
use tokio_stream::StreamExt;

use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET, config::BOOTSTRAP_BLOCK_CHUNK_SIZE},
	contracts::socket::{
		SocketContract::RoundUp,
		SocketInstance,
		Socket_Struct::{Round_Up_Submit, Signatures},
	},
	eth::{BootstrapState, RoundUpEventStatus},
	tx::{TxRequestMetadata, VSPPhase2Metadata},
	utils::{encode_roundup_param, recover_message, sub_display_format},
};

use crate::eth::{
	events::EventMessage,
	send_transaction,
	traits::{BootstrapHandler, Handler},
	ClientMap, EthClient,
};

const SUB_LOG_TARGET: &str = "roundup-handler";

/// The essential task that handles `roundup relay` related events.
pub struct RoundupRelayHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<F, P, T>>,
	/// The receiver that consumes new events from the block channel.
	event_receiver: Receiver<EventMessage>,
	/// `EthClient`s to interact with provided networks except bifrost network.
	external_clients: Arc<ClientMap<F, P, T>>,
	/// Signature of RoundUp Event.
	roundup_signature: B256,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

#[async_trait]
impl<F, P, T> Handler for RoundupRelayHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	async fn run(&mut self) -> Result<()> {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapRoundUpPhase2).await {
				self.bootstrap().await?;

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
					if self.is_target_contract(&log) && self.is_target_event(log.topic0()) {
						self.process_confirmed_log(&log, false).await?;
					}
				}
			}
		}
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) -> Result<()> {
		if let Some(receipt) =
			self.client.get_transaction_receipt(log.transaction_hash.unwrap()).await?
		{
			if !receipt.inner.status() {
				return Ok(());
			}
			match self.decode_log(log.clone()).await {
				Ok(serialized_log) => {
					if !self
						.is_selected_relayer(serialized_log.roundup.round - U256::from(1))
						.await?
					{
						// do nothing if not selected
						return Ok(());
					}

					if !is_bootstrap {
						log::info!(
							target: &self.client.get_chain_name(),
							"-[{}] 👤 RoundUp event detected. ({:?}-{:?})",
							sub_display_format(SUB_LOG_TARGET),
							serialized_log.status,
							log.transaction_hash,
						);
					}

					match RoundUpEventStatus::from_u8(serialized_log.status) {
						RoundUpEventStatus::NextAuthorityCommitted => {
							self.broadcast_roundup(
								&self
									.build_roundup_submit(
										serialized_log.roundup.round,
										serialized_log.roundup.new_relayers,
									)
									.await?,
								is_bootstrap,
							)
							.await?;
						},
						RoundUpEventStatus::NextAuthorityRelayed => return Ok(()),
					}
				},
				Err(e) => {
					let log_msg = format!(
						"-[{}] Error on decoding RoundUp event ({:?}):{}",
						sub_display_format(SUB_LOG_TARGET),
						log.transaction_hash,
						e,
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

	fn is_target_contract(&self, log: &Log) -> bool {
		&log.address() == self.client.protocol_contracts.socket.address()
	}

	fn is_target_event(&self, topic: Option<&B256>) -> bool {
		match topic {
			Some(topic) => topic == &self.roundup_signature,
			None => false,
		}
	}
}

impl<F, P, T> RoundupRelayHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	/// Instantiates a new `RoundupRelayHandler` instance.
	pub fn new(
		client: Arc<EthClient<F, P, T>>,
		event_receiver: Receiver<EventMessage>,
		clients: Arc<ClientMap<F, P, T>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
	) -> Self {
		Self {
			event_receiver,
			client,
			external_clients: clients,
			roundup_signature: RoundUp::SIGNATURE_HASH,
			bootstrap_shared_data,
			handle,
			debug_mode,
		}
	}

	/// Decode & Serialize log to `RoundUp` struct.
	async fn decode_log(&self, log: Log) -> Result<RoundUp> {
		Ok(log.log_decode::<RoundUp>()?.inner.data)
	}

	/// Get the submitted signatures of the updated round.
	async fn get_sorted_signatures(
		&self,
		round: U256,
		new_relayers: &[Address],
	) -> Result<Signatures> {
		let signatures = self
			.client
			.protocol_contracts
			.socket
			.get_round_signatures(round)
			.call()
			.await?
			._0;

		let mut signature_vec = Vec::<PrimitiveSignature>::from(signatures);
		signature_vec
			.sort_by_key(|k| recover_message(*k, &encode_roundup_param(round, &new_relayers)));

		Ok(Signatures::from(signature_vec))
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self, round: U256) -> Result<bool> {
		let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
		Ok(relayer_manager
			.is_previous_selected_relayer(round, self.client.address(), true)
			.call()
			.await?
			._0)
	}

	/// Build `round_control_relay` method call param.
	async fn build_roundup_submit(
		&self,
		round: U256,
		mut new_relayers: Vec<Address>,
	) -> Result<Round_Up_Submit> {
		new_relayers.sort();
		let sigs = self.get_sorted_signatures(round, &new_relayers).await?;
		Ok(Round_Up_Submit { round, new_relayers, sigs })
	}

	/// Build `round_control_relay` method call transaction.
	fn build_transaction_request(
		&self,
		target_socket: &SocketInstance<F, P, T>,
		roundup_submit: &Round_Up_Submit,
	) -> TransactionRequest {
		TransactionRequest::default()
			.to(*target_socket.address())
			.input(TransactionInput::new(
				target_socket.round_control_relay(roundup_submit.clone()).calldata().clone(),
			))
	}

	/// Check roundup submitted before. If not, call `round_control_relay`.
	async fn broadcast_roundup(
		&self,
		roundup_submit: &Round_Up_Submit,
		is_bootstrap: bool,
	) -> Result<()> {
		if self.external_clients.is_empty() {
			return Ok(());
		}

		let mut stream = tokio_stream::iter(self.external_clients.iter());
		while let Some((dst_chain_id, target_client)) = stream.next().await {
			// Check roundup submitted to target chain before.
			let latest_round =
				target_client.protocol_contracts.authority.latest_round().call().await?._0;
			if roundup_submit.round > latest_round {
				let transaction_request = self.build_transaction_request(
					&target_client.protocol_contracts.socket,
					roundup_submit,
				);
				let metadata = TxRequestMetadata::VSPPhase2(VSPPhase2Metadata::new(
					roundup_submit.round,
					*dst_chain_id,
				));

				if is_bootstrap {
					target_client
						.sync_send_transaction(
							transaction_request,
							SUB_LOG_TARGET.to_string(),
							metadata,
						)
						.await?
				} else {
					send_transaction(
						target_client.clone(),
						transaction_request,
						SUB_LOG_TARGET.to_string(),
						metadata,
						self.debug_mode,
						self.handle.clone(),
					);
				}
			}
		}

		Ok(())
	}

	/// Check if external clients are in the latest round.
	async fn wait_if_latest_round(&self) -> Result<()> {
		let barrier_clone = self.bootstrap_shared_data.roundup_barrier.clone();
		let external_clients = &self.external_clients;

		for (_, target_client) in external_clients.iter() {
			let barrier_clone_inner = barrier_clone.clone();
			let current_round =
				self.client.protocol_contracts.authority.latest_round().call().await?._0;
			let target_chain_round =
				target_client.protocol_contracts.authority.latest_round().call().await?._0;

			let bootstrap_guard = self.bootstrap_shared_data.roundup_bootstrap_count.clone();

			tokio::spawn(async move {
				if current_round == target_chain_round {
					*bootstrap_guard.lock().await += 1;
				}
				barrier_clone_inner.wait().await;
			});
		}

		Ok(())
	}
}

#[async_trait]
impl<F, P, T> BootstrapHandler for RoundupRelayHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping RoundUp events.",
			sub_display_format(SUB_LOG_TARGET),
		);

		let mut bootstrap_guard = self.bootstrap_shared_data.bootstrap_states.write().await;
		// Checking if the current round is the latest round
		self.wait_if_latest_round().await?;

		// Wait to lock after checking if it is latest round
		self.bootstrap_shared_data.roundup_barrier.clone().wait().await;

		// if all of chain is the latest round already
		if *self.bootstrap_shared_data.roundup_bootstrap_count.lock().await
			== self.external_clients.len() as u8
		{
			// set all of state to BootstrapSocket
			for state in bootstrap_guard.iter_mut() {
				*state = BootstrapState::BootstrapSocketRelay;
			}
		}

		if bootstrap_guard.iter().all(|s| *s == BootstrapState::BootstrapRoundUpPhase2) {
			drop(bootstrap_guard);
			let logs = self.get_bootstrap_events().await?;
			for log in logs {
				self.process_confirmed_log(&log, true).await?;
			}
		}

		// Poll socket barrier to call wait()
		let socket_barrier_clone = self.bootstrap_shared_data.socket_barrier.clone();

		tokio::spawn(async move {
			socket_barrier_clone.clone().wait().await;
		});

		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<Log>> {
		let mut logs = vec![];

		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			let bootstrap_offset_height = self
				.client
				.get_bootstrap_offset_height_based_on_block_time(
					bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
					self.client.protocol_contracts.authority.round_info().call().await?._0,
				)
				.await?;

			let latest_block_number = self.client.get_block_number().await?;
			let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
			let to_block = latest_block_number;

			// Split from_block into smaller chunks
			while from_block <= to_block {
				let chunk_to_block =
					std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

				let filter = Filter::new()
					.address(*self.client.protocol_contracts.socket.address())
					.event_signature(self.roundup_signature)
					.from_block(from_block)
					.to_block(chunk_to_block);

				let chunk_logs = self.client.get_logs(&filter).await?;
				logs.extend(chunk_logs);

				from_block = chunk_to_block + 1;
			}
		}

		Ok(logs)
	}
}
