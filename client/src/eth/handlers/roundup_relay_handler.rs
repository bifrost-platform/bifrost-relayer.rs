use std::{
	collections::{BTreeMap, HashMap},
	sync::{Arc, Mutex},
	time::Duration,
};

use alloy::{
	network::{Network, primitives::ReceiptResponse as _},
	primitives::{Address, B256, ChainId, Signature, U256},
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
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET,
		config::BOOTSTRAP_BLOCK_CHUNK_SIZE,
		tx::{DEFAULT_CALL_RETRY_INTERVAL_MS, ROUNDUP_RELAY_RETRY_INTERVAL_MS},
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

use crate::{
	eth::{
		ClientMap, EthClient,
		events::EventMessage,
		send_transaction,
		traits::{BootstrapHandler, Handler},
	},
	sol::{
		client::SolClient,
		codec::RoundUpSubmit as SolRoundUpSubmit,
		convert::build_sol_round_up_submit,
		handlers::outbound::{SolOutboundJob, SolOutboundSender},
	},
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
	/// Pending roundup relays per external chain, stored as a `Vec` sorted by round ascending
	/// so that the lowest (oldest) rounds are relayed first on retry.
	/// Uses a plain `Mutex` (no `Arc`) for interior mutability — the lock is always released
	/// before any `.await`, so there is no deadlock risk in this single-handler context.
	pending_relays: Mutex<HashMap<ChainId, Vec<Round_Up_Submit>>>,
	/// Per-Solana-cluster `SolClient`s keyed by CCCP `ChainId`, used to probe
	/// on-chain `socket_config.latest_round_id` for `broadcast_roundup`
	/// skip-if-already-synced logic and for retry cleanup.
	sol_clients: Arc<BTreeMap<ChainId, SolClient>>,
	/// Per-Solana-cluster outbound job senders. Mirrors
	/// `SocketRelayHandler.sol_outbound_senders`: empty if no Solana
	/// clusters are configured, in which case every Solana branch in this
	/// handler is a no-op.
	sol_outbound_senders: Arc<BTreeMap<ChainId, SolOutboundSender>>,
	/// Pending `RoundUpSubmit` queue per Solana cluster. Same sort+dedupe
	/// contract as `pending_relays`, but against the borsh-encodable
	/// variant so the retry loop can re-send without re-converting from
	/// the EVM shape. Entries are dropped once the cluster's on-chain
	/// `latest_round_id` catches up to Bifrost's.
	pending_sol_relays: Mutex<HashMap<ChainId, Vec<SolRoundUpSubmit>>>,
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

		let mut retry_interval = tokio::time::interval(tokio::time::Duration::from_millis(
			ROUNDUP_RELAY_RETRY_INTERVAL_MS,
		));
		// Skip the immediate first tick so we don't retry before any failure has occurred.
		retry_interval.tick().await;

		loop {
			tokio::select! {
				msg = self.event_stream.next() => {
					match msg {
						Some(Ok(msg)) => {
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
						},
						_ => {},
					}
				},
				_ = retry_interval.tick() => {
					if !self.pending_relays.lock().unwrap().is_empty() {
						self.retry_pending_relays().await?;
					}
				},
			}
		}
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
							let roundup_submit = self
								.build_roundup_submit(
									serialized_log.roundup.round,
									serialized_log.roundup.new_relayers,
								)
								.await?;
							self.broadcast_roundup(roundup_submit, relay_as, is_bootstrap).await?;
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
		sol_clients: Arc<BTreeMap<ChainId, SolClient>>,
		sol_outbound_senders: Arc<BTreeMap<ChainId, SolOutboundSender>>,
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
			pending_relays: Mutex::new(HashMap::new()),
			sol_clients,
			sol_outbound_senders,
			pending_sol_relays: Mutex::new(HashMap::new()),
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

		let mut keyed = Vec::<Signature>::from(signatures)
			.into_iter()
			.map(|sig| {
				recover_message(sig, &encode_roundup_param(round, new_relayers))
					.map(|addr| (addr, sig))
			})
			.collect::<Result<Vec<_>, _>>()?;
		keyed.sort_by_key(|(addr, _)| *addr);

		Ok(Signatures::from(keyed.into_iter().map(|(_, sig)| sig).collect::<Vec<_>>()))
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

	/// Broadcasts a `round_control_relay` to all external chains.
	///
	/// For the non-bootstrap path, the `roundup_submit` is always stored in `pending_relays`
	/// before attempting the immediate send, so the 10-minute retry interval can catch any
	/// chains that were out-of-sync at the time of the original event.
	///
	/// - Bootstrap path (`is_bootstrap=true`): errors propagate immediately; no pending storage.
	/// - Normal path (`is_bootstrap=false`): fire-and-forget send; per-chain RPC failures are
	///   logged and the chain stays in `pending_relays` for later retry.
	async fn broadcast_roundup(
		&self,
		roundup_submit: Round_Up_Submit,
		from: Address,
		is_bootstrap: bool,
	) -> Result<()> {
		if !self.external_clients.is_empty() {
			for (dst_chain_id, target_client) in self.external_clients.iter() {
				// For the normal path, always record this relay before attempting to send so that
				// the retry interval can pick it up if the chain is out-of-sync.
				if !is_bootstrap {
					let mut pending = self.pending_relays.lock().unwrap();
					let relays = pending.entry(*dst_chain_id).or_default();
					if !relays.iter().any(|r| r.round == roundup_submit.round) {
						let pos = relays.partition_point(|r| r.round < roundup_submit.round);
						relays.insert(pos, roundup_submit.clone());
					}
				}

				let latest_round =
					target_client.protocol_contracts.authority.latest_round().call().await?;
				if roundup_submit.round > latest_round {
					let transaction_request = self.build_transaction_request(
						&target_client.protocol_contracts.socket,
						&roundup_submit,
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
		}

		// Solana lane — disjoint from the EVM loop above because the Solana
		// clusters are not present in `external_clients` (`is_native==false`
		// only collects EVM). Failures here never abort the EVM path; a
		// misconfigured Solana cluster must not block roundup dissemination
		// to every other chain.
		self.broadcast_roundup_sol(&roundup_submit, is_bootstrap).await;

		Ok(())
	}

	/// Per-Solana-cluster dispatch for the `round_control_relay` rotation.
	///
	/// Mirrors the EVM loop in `broadcast_roundup`:
	///   * record the submit in `pending_sol_relays` (dedup + sorted asc)
	///     so the retry interval can pick it up if this cluster is
	///     out-of-sync,
	///   * probe `SolClient::latest_round_id` to skip the send when the
	///     cluster is already caught up,
	///   * enqueue a `SolOutboundJob::RoundControlRelay` onto the cluster's
	///     outbound queue.
	///
	/// All per-cluster errors (conversion, RPC probe failure, channel
	/// closed) are logged and swallowed — the pending queue plus the
	/// retry tick provide the recovery path. The bootstrap flag only
	/// affects logging here; unlike the EVM path, the Solana outbound
	/// handler does its own confirmation + retry with priority-fee
	/// escalation, so there is no `sync_send_transaction` analogue to
	/// gate bootstrap errors.
	async fn broadcast_roundup_sol(&self, roundup_submit: &Round_Up_Submit, is_bootstrap: bool) {
		if self.sol_outbound_senders.is_empty() {
			return;
		}

		let sol_submit = match build_sol_round_up_submit(roundup_submit) {
			Ok(s) => s,
			Err(err) => {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					"❗️ Failed to convert Round_Up_Submit for Solana dispatch: {err}"
				);
				return;
			},
		};

		for (dst_chain_id, sender) in self.sol_outbound_senders.iter() {
			// Always record first so the retry interval can recover from a
			// transient send failure. Dedupe by borsh-encoded `round`.
			if !is_bootstrap {
				let mut pending = self.pending_sol_relays.lock().unwrap();
				let relays = pending.entry(*dst_chain_id).or_default();
				if !relays.iter().any(|r| r.round == sol_submit.round) {
					let pos = relays.partition_point(|r| r.round < sol_submit.round);
					relays.insert(pos, sol_submit.clone());
				}
			}

			// Probe the cluster's current round to avoid a guaranteed-to-revert
			// `round_control_relay` (the on-chain constraint is
			// `submit.round - 1 == socket_config.latest_round_id`). If the
			// probe fails the entry stays in `pending_sol_relays` so the
			// retry tick picks it up once the RPC recovers.
			let Some(sol_client) = self.sol_clients.get(dst_chain_id) else {
				log::debug!(
					target: &self.client.get_chain_name(),
					"-[{}] no SolClient registered for dst {dst_chain_id}; skipping round {:?}",
					sub_display_format(SUB_LOG_TARGET),
					roundup_submit.round,
				);
				continue;
			};

			let cluster_latest = match sol_client.latest_round_id().await {
				Ok(v) => v,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] (sol-roundup) latest_round_id probe failed for cluster {dst_chain_id}: {err}",
						sub_display_format(SUB_LOG_TARGET),
					);
					continue;
				},
			};
			let submit_round = u64_from_round_be(&sol_submit.round);
			if submit_round <= cluster_latest {
				// Already synced or stale — nothing to do; drop from pending.
				let mut pending = self.pending_sol_relays.lock().unwrap();
				if let Some(relays) = pending.get_mut(dst_chain_id) {
					relays.retain(|r| u64_from_round_be(&r.round) > cluster_latest);
					if relays.is_empty() {
						pending.remove(dst_chain_id);
					}
				}
				continue;
			}

			// Only send if this submit is the immediate next round. On-chain
			// `round_control_relay` rejects anything except
			// `submit.round - 1 == latest_round_id`; sending the wrong one
			// just burns a tx and creates a revert the pending retry tick
			// has to retract. The retry loop will catch up the intermediate
			// rounds in order.
			if submit_round != cluster_latest + 1 {
				continue;
			}

			let job = SolOutboundJob::RoundControlRelay { submit: sol_submit.clone() };
			match sender.send(job) {
				Ok(()) => log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 📤 (sol-roundup) enqueued round {} for cluster {dst_chain_id}{}",
					sub_display_format(SUB_LOG_TARGET),
					submit_round,
					if is_bootstrap { " [bootstrap]" } else { "" },
				),
				Err(err) => br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					"❗️ Failed to enqueue Solana roundup for cluster {dst_chain_id}: {err}"
				),
			}
		}
	}

	/// Retries all pending roundup relays. Called every 10 minutes via interval tick.
	///
	/// For each external chain with pending relays, fetches its current round and:
	/// - Drops the entire chain entry if the chain is already synced to the bifrost latest round.
	/// - Otherwise relays all pending rounds (ascending) whose round exceeds the chain's current round.
	///
	/// The Solana lane (`retry_pending_sol_relays`) runs immediately after
	/// the EVM lane and uses the same tick source. Its failure modes are
	/// strictly per-cluster and never abort the EVM lane.
	async fn retry_pending_relays(&self) -> Result<()> {
		let bifrost_latest_round =
			self.client.protocol_contracts.authority.latest_round().call().await?;

		let chain_ids: Vec<ChainId> = self.pending_relays.lock().unwrap().keys().cloned().collect();

		for dst_chain_id in chain_ids {
			let target_client = match self.external_clients.get(&dst_chain_id) {
				Some(c) => c.clone(),
				None => {
					self.pending_relays.lock().unwrap().remove(&dst_chain_id);
					continue;
				},
			};

			let chain_latest_round =
				target_client.protocol_contracts.authority.latest_round().call().await?;

			// Drop all pending entries for this chain if it is already synced.
			if chain_latest_round == bifrost_latest_round {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] ✅ Chain {} already synced to round {:?}. Dropping pending relays.",
					sub_display_format(SUB_LOG_TARGET),
					dst_chain_id,
					chain_latest_round,
				);
				self.pending_relays.lock().unwrap().remove(&dst_chain_id);
				continue;
			}

			// Collect only the rounds that still need to be relayed (ascending order).
			let pending_submits: Vec<Round_Up_Submit> = self
				.pending_relays
				.lock()
				.unwrap()
				.get(&dst_chain_id)
				.map(|v| v.iter().filter(|r| r.round > chain_latest_round).cloned().collect())
				.unwrap_or_default();

			for roundup_submit in pending_submits {
				let from = self.relay_as(roundup_submit.round - U256::from(1)).await;

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔄 Retrying RoundUp relay to chain {} for round {:?}",
					sub_display_format(SUB_LOG_TARGET),
					dst_chain_id,
					roundup_submit.round,
				);

				let transaction_request = self.build_transaction_request(
					&target_client.protocol_contracts.socket,
					&roundup_submit,
					from,
				);
				let metadata = Arc::new(VSPPhase2Metadata::new(roundup_submit.round, dst_chain_id));
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

		// Mirror the above pattern for each configured Solana cluster.
		self.retry_pending_sol_relays().await;

		Ok(())
	}

	/// Solana counterpart to `retry_pending_relays`. Called on the same
	/// `ROUNDUP_RELAY_RETRY_INTERVAL_MS` tick.
	///
	/// For each cluster with pending entries:
	///   * drop the whole entry if `socket_config.latest_round_id == Bifrost
	///     latest_round` — the cluster has caught up,
	///   * otherwise drop entries whose round is already ≤ the cluster's
	///     latest and re-enqueue the immediate next round
	///     (`latest_round_id + 1`). The on-chain `round_control_relay`
	///     rejects any other target, so submitting a later round here
	///     would just revert.
	///
	/// Never returns an error. Per-cluster RPC failures are logged and
	/// leave the pending entries untouched so the next tick can retry.
	async fn retry_pending_sol_relays(&self) {
		if self.pending_sol_relays.lock().unwrap().is_empty() {
			return;
		}

		let bifrost_latest =
			match self.client.protocol_contracts.authority.latest_round().call().await {
				Ok(v) => v,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] (sol-roundup retry) bifrost latest_round probe failed: {err}",
						sub_display_format(SUB_LOG_TARGET),
					);
					return;
				},
			};
		let bifrost_latest_u64 = u64_from_u256_low(bifrost_latest);

		let chain_ids: Vec<ChainId> =
			self.pending_sol_relays.lock().unwrap().keys().cloned().collect();

		for dst_chain_id in chain_ids {
			let Some(sol_client) = self.sol_clients.get(&dst_chain_id).cloned() else {
				self.pending_sol_relays.lock().unwrap().remove(&dst_chain_id);
				continue;
			};
			let Some(sender) = self.sol_outbound_senders.get(&dst_chain_id).cloned() else {
				self.pending_sol_relays.lock().unwrap().remove(&dst_chain_id);
				continue;
			};

			let cluster_latest = match sol_client.latest_round_id().await {
				Ok(v) => v,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] 🔄 (sol-roundup retry) latest_round_id probe failed for cluster {dst_chain_id}: {err}",
						sub_display_format(SUB_LOG_TARGET),
					);
					continue;
				},
			};

			if cluster_latest == bifrost_latest_u64 {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] ✅ Solana cluster {dst_chain_id} already synced to round {cluster_latest}. Dropping pending relays.",
					sub_display_format(SUB_LOG_TARGET),
				);
				self.pending_sol_relays.lock().unwrap().remove(&dst_chain_id);
				continue;
			}

			// Garbage-collect entries the cluster already passed, then
			// pick out the immediate-next-round submit if we still have
			// it. The on-chain IX rejects `submit.round != latest+1`, so
			// there is no gain in enqueuing anything beyond that.
			let next_submit: Option<SolRoundUpSubmit> = {
				let mut pending = self.pending_sol_relays.lock().unwrap();
				if let Some(relays) = pending.get_mut(&dst_chain_id) {
					relays.retain(|r| u64_from_round_be(&r.round) > cluster_latest);
					if relays.is_empty() {
						pending.remove(&dst_chain_id);
						None
					} else {
						relays
							.iter()
							.find(|r| u64_from_round_be(&r.round) == cluster_latest + 1)
							.cloned()
					}
				} else {
					None
				}
			};

			let Some(submit) = next_submit else { continue };
			let submit_round = u64_from_round_be(&submit.round);

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔄 Retrying Solana RoundUp relay to cluster {dst_chain_id} for round {submit_round}",
				sub_display_format(SUB_LOG_TARGET),
			);

			let job = SolOutboundJob::RoundControlRelay { submit };
			if let Err(err) = sender.send(job) {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					"❗️ (sol-roundup retry) enqueue failed for cluster {dst_chain_id}: {err}"
				);
			}
		}
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

	/// Enqueue `round_control_relay` jobs for every round missing on each
	/// configured Solana cluster, in strictly ascending order. Pairs with
	/// `wait_if_sol_latest_round` which blocks until the jobs land.
	///
	/// The on-chain `round_control_relay` constraint is
	/// `submit.round - 1 == latest_round_id`, so rounds MUST be submitted
	/// in order. The outbound worker serializes jobs via its single
	/// `recv()` loop with per-tx confirmation, so enqueuing them FIFO is
	/// sufficient to preserve that order.
	///
	/// Per-cluster failures (RPC probe, signature fetch, channel closed)
	/// are logged and skip that cluster — we never abort the bootstrap
	/// for another cluster because one misbehaved. `wait_if_sol_latest_round`
	/// will time out and surface the same cluster later.
	async fn bootstrap_sol_catchup(&self) -> Result<()> {
		if self.sol_clients.is_empty() {
			return Ok(());
		}

		let bifrost_latest = self.client.protocol_contracts.authority.latest_round().call().await?;
		let bifrost_latest_u64 = u64_from_u256_low(bifrost_latest);

		for (chain_id, sol_client) in self.sol_clients.iter() {
			let Some(sender) = self.sol_outbound_senders.get(chain_id) else {
				continue;
			};

			let cluster_latest = match sol_client.latest_round_id().await {
				Ok(v) => v,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚙️  [Bootstrap] Solana cluster {chain_id} latest_round_id probe failed: {err}",
						sub_display_format(SUB_LOG_TARGET),
					);
					continue;
				},
			};

			if cluster_latest >= bifrost_latest_u64 {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚙️  [Bootstrap] Solana cluster {chain_id} already at round {cluster_latest}",
					sub_display_format(SUB_LOG_TARGET),
				);
				continue;
			}

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚙️  [Bootstrap] Solana cluster {chain_id} catching up: {cluster_latest} → {bifrost_latest_u64}",
				sub_display_format(SUB_LOG_TARGET),
			);

			for round_u64 in (cluster_latest + 1)..=bifrost_latest_u64 {
				let round = U256::from(round_u64);
				let relayer_manager =
					self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
				let new_relayers = match relayer_manager
					.previous_selected_relayers(round, true)
					.call()
					.await
				{
					Ok(mut addrs) => {
						addrs.sort();
						addrs
					},
					Err(err) => {
						log::warn!(
							target: &self.client.get_chain_name(),
							"-[{}] ⚙️  [Bootstrap] previous_selected_relayers({round_u64}) for cluster {chain_id} failed: {err}",
							sub_display_format(SUB_LOG_TARGET),
						);
						break;
					},
				};
				let submit = match self.build_roundup_submit(round, new_relayers).await {
					Ok(s) => s,
					Err(err) => {
						log::warn!(
							target: &self.client.get_chain_name(),
							"-[{}] ⚙️  [Bootstrap] build_roundup_submit({round_u64}) for cluster {chain_id} failed: {err}",
							sub_display_format(SUB_LOG_TARGET),
						);
						break;
					},
				};
				let sol_submit = match build_sol_round_up_submit(&submit) {
					Ok(s) => s,
					Err(err) => {
						log::warn!(
							target: &self.client.get_chain_name(),
							"-[{}] ⚙️  [Bootstrap] RoundUpSubmit conversion for cluster {chain_id} round {round_u64} failed: {err}",
							sub_display_format(SUB_LOG_TARGET),
						);
						break;
					},
				};

				if let Err(err) =
					sender.send(SolOutboundJob::RoundControlRelay { submit: sol_submit })
				{
					br_primitives::log_and_capture!(
						error,
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						"❗️ (sol-roundup bootstrap) enqueue failed for cluster {chain_id} round {round_u64}: {err}"
					);
					break;
				}
			}
		}

		Ok(())
	}

	/// Block the bootstrap path until every configured Solana cluster has
	/// caught up to Bifrost's `latest_round_id`. No-op if no clusters are
	/// configured.
	///
	/// Polls `socket_config.latest_round_id` on each cluster at
	/// `DEFAULT_CALL_RETRY_INTERVAL_MS`, the same cadence the EVM
	/// `wait_if_latest_round` uses, until every cluster matches. The
	/// outbound worker's confirmation loop is what makes forward progress
	/// visible to this check.
	async fn wait_if_sol_latest_round(&self) -> Result<()> {
		if self.sol_clients.is_empty() {
			return Ok(());
		}

		let bifrost_authority = self.client.protocol_contracts.authority.clone();

		loop {
			let bifrost_latest = bifrost_authority.latest_round().call().await?;
			let bifrost_latest_u64 = u64_from_u256_low(bifrost_latest);

			let mut all_synced = true;
			for (chain_id, sol_client) in self.sol_clients.iter() {
				match sol_client.latest_round_id().await {
					Ok(cluster_latest) => {
						if cluster_latest < bifrost_latest_u64 {
							log::info!(
								target: &self.client.get_chain_name(),
								"-[{}] ⚙️  [Bootstrap] waiting on Solana cluster {chain_id}: {cluster_latest}/{bifrost_latest_u64}",
								sub_display_format(SUB_LOG_TARGET),
							);
							all_synced = false;
						}
					},
					Err(err) => {
						log::warn!(
							target: &self.client.get_chain_name(),
							"-[{}] ⚙️  [Bootstrap] Solana cluster {chain_id} latest_round_id probe failed: {err}",
							sub_display_format(SUB_LOG_TARGET),
						);
						all_synced = false;
					},
				}
			}
			if all_synced {
				return Ok(());
			}
			tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
		}
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

		// Solana: enqueue any missing rounds for every configured cluster.
		// The outbound worker processes them FIFO, so ordering is preserved.
		self.bootstrap_sol_catchup().await?;

		// Checking if the current round is the latest round
		self.wait_if_latest_round().await?;

		// Block until every Solana cluster has also caught up. Runs after
		// `wait_if_latest_round` so EVM barrier spawns are already in
		// flight concurrently.
		self.wait_if_sol_latest_round().await?;

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
					BootstrapState::BootstrapSocketRelayQueue;
			}
		}

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] BootstrapRoundUpPhase2 → BootstrapSocketRelayQueue",
			sub_display_format(SUB_LOG_TARGET),
		);
		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<Log>> {
		// Reuse logs cached by RoundupEmitter (which runs first in BootstrapRoundUpPhase1)
		// to avoid a duplicate eth_getLogs call for the same block range.
		if let Some(cached) = self.bootstrap_shared_data.take_bootstrap_roundup_logs().await {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚙️  [Bootstrap mode] Reusing {} cached RoundUp events (no duplicate eth_getLogs)",
				sub_display_format(SUB_LOG_TARGET),
				cached.len(),
			);
			return Ok(cached);
		}

		// Fallback: fetch directly when no cache is available.
		log::warn!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Cache miss – fetching RoundUp events directly",
			sub_display_format(SUB_LOG_TARGET),
		);
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

// ---------------------------------------------------------------------------
// Round-id helpers
// ---------------------------------------------------------------------------

/// Read the low 8 bytes of a borsh-encoded uint256 round (big-endian).
/// Mirror of `cccp-solana::socket::roundup::round_as_u64` — the CCCP
/// protocol only allocates 64 bits of round id, so any high bits being
/// set is a programming error. This panics in debug if the upper 24
/// bytes are non-zero; in release it silently truncates (matches the
/// existing `round_id_from_submit` in the IX builder).
fn u64_from_round_be(round: &[u8; 32]) -> u64 {
	debug_assert!(
		round[0..24].iter().all(|&b| b == 0),
		"round uint256 has non-zero high bits; CCCP only uses 64",
	);
	let mut buf = [0u8; 8];
	buf.copy_from_slice(&round[24..32]);
	u64::from_be_bytes(buf)
}

/// Downcast a Bifrost `U256` round to `u64`. The CCCP protocol treats
/// `round_id` as a `u64` on Solana, so any round beyond `u64::MAX`
/// is unreachable in practice. Reading the low limb is equivalent to
/// truncation and matches `u64_from_round_be` after a borsh encode.
fn u64_from_u256_low(value: U256) -> u64 {
	value.as_limbs()[0]
}
