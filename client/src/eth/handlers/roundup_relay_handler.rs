use std::{sync::Arc, time::Duration};

use alloy::{
	network::{Network, primitives::ReceiptResponse as _},
	primitives::{Address, B256, Signature, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::Log,
	sol_types::SolEvent as _,
};
use async_trait::async_trait;
use eyre::Result;
use sc_service::SpawnTaskHandle;
use tokio::sync::broadcast::Receiver;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET, config::BOOTSTRAP_BLOCK_CHUNK_SIZE,
		tx::DEFAULT_CALL_RETRY_INTERVAL_MS,
	},
	contracts::socket::{
		Socket_Struct::{Round_Up_Submit, Signatures},
		SocketContract::RoundUp,
		SocketInstance,
	},
	eth::{BootstrapState, RoundUpEventStatus},
	tx::VSPPhase2Metadata,
	utils::{encode_roundup_param, recover_message, sub_display_format},
};

use crate::eth::{
	ClientMap, EthClient,
	events::EventMessage,
	send_transaction,
	traits::{BootstrapHandler, Handler},
};

const SUB_LOG_TARGET: &str = "roundup-handler";

/// The essential task that handles `roundup relay` related events.
pub struct RoundupRelayHandler<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<F, P, N>>,
	/// The receiver that consumes new events from the block channel.
	event_stream: BroadcastStream<EventMessage>,
	/// `EthClient`s to interact with provided networks except bifrost network.
	external_clients: Arc<ClientMap<F, P, N>>,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

#[async_trait]
impl<F, P, N: Network> Handler for RoundupRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
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

			for log in msg.event_logs {
				if self.is_target_contract(&log) && self.is_target_event(log.topic0()) {
					self.process_confirmed_log(&log, false).await?;
				}
			}
		}

		Ok(())
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) -> Result<()> {
		if let Some(receipt) =
			self.client.get_transaction_receipt(log.transaction_hash.unwrap()).await?
		{
			if !receipt.status() {
				return Ok(());
			}
			match self.decode_log(log.clone()).await {
				Ok(serialized_log) => {
					let prev_round = serialized_log.roundup.round - U256::from(1);
					let relay_as = self.relay_as(prev_round).await;
					if !self.is_selected_relayer(prev_round, relay_as).await? {
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
								relay_as,
								is_bootstrap,
							)
							.await?;
						},
						RoundUpEventStatus::NextAuthorityRelayed => return Ok(()),
					}
				},
				Err(e) => {
					br_primitives::log_and_capture!(
						error,
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						"Error on decoding RoundUp event ({:?}):{}",
						log.transaction_hash,
						e
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
			Some(topic) => topic == &RoundUp::SIGNATURE_HASH,
			None => false,
		}
	}
}

impl<F, P, N: Network> RoundupRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// Instantiates a new `RoundupRelayHandler` instance.
	pub fn new(
		client: Arc<EthClient<F, P, N>>,
		event_receiver: Receiver<EventMessage>,
		clients: Arc<ClientMap<F, P, N>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
	) -> Self {
		let external_clients = Arc::new(
			clients
				.iter()
				.filter_map(|(id, client)| {
					if !client.metadata.is_native { Some((*id, client.clone())) } else { None }
				})
				.collect::<ClientMap<F, P, N>>(),
		);

		Self {
			event_stream: BroadcastStream::new(event_receiver),
			client,
			external_clients,
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
		let signatures =
			self.client.protocol_contracts.socket.get_round_signatures(round).call().await?;

		let mut signature_vec = Vec::<Signature>::from(signatures);
		signature_vec
			.sort_by_key(|k| recover_message(*k, &encode_roundup_param(round, new_relayers)));

		Ok(Signatures::from(signature_vec))
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self, round: U256, relayer: Address) -> Result<bool> {
		let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
		Ok(relayer_manager
			.is_previous_selected_relayer(round, relayer, true)
			.call()
			.await?)
	}

	async fn relay_as(&self, round: U256) -> Address {
		let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
		let prev_relayers =
			relayer_manager.previous_selected_relayers(round, true).call().await.unwrap();
		let signers = self.client.signers();

		signers.into_iter().find(|s| prev_relayers.contains(s)).unwrap_or_default()
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
		target_socket: &SocketInstance<F, P, N>,
		roundup_submit: &Round_Up_Submit,
		from: Address,
	) -> N::TransactionRequest {
		target_socket
			.round_control_relay(roundup_submit.clone())
			.from(from)
			.into_transaction_request()
	}

	/// Check roundup submitted before. If not, call `round_control_relay`.
	async fn broadcast_roundup(
		&self,
		roundup_submit: &Round_Up_Submit,
		from: Address,
		is_bootstrap: bool,
	) -> Result<()> {
		if self.external_clients.is_empty() {
			return Ok(());
		}

		for (dst_chain_id, target_client) in self.external_clients.iter() {
			// Check roundup submitted to target chain before.
			let latest_round =
				target_client.protocol_contracts.authority.latest_round().call().await?;
			if roundup_submit.round > latest_round {
				let transaction_request = self.build_transaction_request(
					&target_client.protocol_contracts.socket,
					roundup_submit,
					from,
				);
				let metadata =
					Arc::new(VSPPhase2Metadata::new(roundup_submit.round, *dst_chain_id));

				if is_bootstrap {
					while let Err(e) = target_client
						.sync_send_transaction(
							transaction_request.clone(),
							SUB_LOG_TARGET.to_string(),
							metadata.clone(),
						)
						.await
					{
						if e.to_string().to_lowercase().contains("nonce too low") {
							target_client.flush_stalled_transactions().await?;
							continue;
						} else {
							eyre::bail!(e);
						}
					}
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
		let external_clients = &self.external_clients;

		for (_, target_client) in external_clients.iter() {
			let this_roundup_barrier = self.bootstrap_shared_data.roundup_barrier.clone();
			let bifrost_authority = self.client.protocol_contracts.authority.clone();
			let target_authority = target_client.protocol_contracts.authority.clone();

			tokio::spawn(async move {
				while target_authority.latest_round().call().await.unwrap()
					< bifrost_authority.latest_round().call().await.unwrap()
				{
					tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
				}

				this_roundup_barrier.wait().await;
			});
		}

		Ok(())
	}
}

#[async_trait]
impl<F, P, N: Network> BootstrapHandler for RoundupRelayHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	fn get_chain_id(&self) -> u64 {
		self.client.metadata.id
	}

	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		self.wait_for_bootstrap_state(BootstrapState::BootstrapRoundUpPhase2).await?;

		// Fetch roundup events
		let logs = self.get_bootstrap_events().await?;
		for log in logs {
			// Process roundup events
			self.process_confirmed_log(&log, true).await?;
		}

		// Checking if the current round is the latest round
		self.wait_if_latest_round().await?;

		// Wait to lock after checking if it is latest round
		self.bootstrap_shared_data.roundup_barrier.clone().wait().await;

		// set all chains except bitcoin to BootstrapSocketRelay
		let chain_ids: Vec<_> = {
			let bootstrap_states = self.bootstrap_shared_data.bootstrap_states.read().await;
			bootstrap_states
				.keys()
				.filter(|chain_id| **chain_id != self.client.get_bitcoin_chain_id().unwrap())
				.cloned()
				.collect()
		};
		if !chain_ids.is_empty() {
			let mut bootstrap_states = self.bootstrap_shared_data.bootstrap_states.write().await;
			for chain_id in chain_ids {
				*bootstrap_states.get_mut(&chain_id).unwrap() =
					BootstrapState::BootstrapSocketRelay;
			}
		}

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] BootstrapRoundUpPhase2 → BootstrapSocketRelay",
			sub_display_format(SUB_LOG_TARGET),
		);
		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<Log>> {
		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			self.client
				.get_historical_logs(
					bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
					vec![*self.client.protocol_contracts.socket.address()],
					RoundUp::SIGNATURE_HASH,
					BOOTSTRAP_BLOCK_CHUNK_SIZE,
				)
				.await
		} else {
			Ok(vec![])
		}
	}
}
