use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
	time::{Duration, Instant},
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

use crate::eth::{
	ClientMap, EthClient,
	events::EventMessage,
	send_transaction,
	traits::{BootstrapHandler, Handler},
};

const SUB_LOG_TARGET: &str = "roundup-handler";

/// How long a queued RoundUp event is retried before it is dropped as unprocessable.
/// Guards against an unbounded `pending_events` queue when a transaction receipt never
/// becomes available (e.g. the emitting transaction was reorged out). Chosen well above
/// any realistic RPC outage so a transient failure never drops a pending relay.
const PENDING_EVENT_MAX_AGE: Duration = Duration::from_secs(30 * 60);

/// A RoundUp event log waiting for enough block confirmations on the bifrost chain
/// before it is processed.
struct PendingRoundUpEvent {
	/// The RoundUp event log.
	log: Log,
	/// Block number reported by the `EventManager` when the event was received.
	block_number: u64,
	/// When the event was first queued, used as a wall-clock safety cap.
	first_seen: Instant,
}

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
	/// RoundUp event logs waiting for enough block confirmations on the bifrost chain
	/// before they are processed. Guarded by a plain `Mutex`; the lock is always released
	/// before any `.await`.
	pending_events: Mutex<Vec<PendingRoundUpEvent>>,
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

		// Timer that drains `pending_events` once the emitting blocks have enough confirmations.
		let mut confirm_interval = tokio::time::interval(tokio::time::Duration::from_millis(
			self.client.metadata.call_interval,
		));

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

							let mut queued = 0u32;
							for log in msg.event_logs {
								if self.is_target_contract(&log) && self.is_target_event(log.topic0()) {
									self.pending_events.lock().unwrap().push(PendingRoundUpEvent {
										log,
										block_number: msg.block_number,
										first_seen: Instant::now(),
									});
									queued += 1;
								}
							}
							if queued > 0 {
								log::info!(
									target: &self.client.get_chain_name(),
									"-[{}] 📦 Queued {} RoundUp event(s) from #{:?} for confirmation",
									sub_display_format(SUB_LOG_TARGET),
									queued,
									msg.block_number,
								);
							}
						},
						Some(Err(e)) => {
							log::warn!(
								target: &self.client.get_chain_name(),
								"-[{}] ⚠️ RoundUp event stream lagged, some blocks skipped: {:?}",
								sub_display_format(SUB_LOG_TARGET),
								e,
							);
						},
						None => break,
					}
				},
				_ = confirm_interval.tick() => {
					if let Err(e) = self.process_confirmed_pending_events().await {
						br_primitives::log_and_capture!(
							error,
							&self.client.get_chain_name(),
							SUB_LOG_TARGET,
							"❗️ Error processing confirmed RoundUp events: {:?}",
							e
						);
					}
				},
				_ = retry_interval.tick() => {
					if let Err(e) = self.retry_pending_relays().await {
						br_primitives::log_and_capture!(
							error,
							&self.client.get_chain_name(),
							SUB_LOG_TARGET,
							"❗️ Error retrying pending roundup relays: {:?}",
							e
						);
					}
				},
			}
		}

		Ok(())
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) -> Result<()> {
		let tx_hash = log.transaction_hash.unwrap();
		let receipt = match self.client.get_transaction_receipt(tx_hash).await? {
			Some(receipt) => receipt,
			None => {
				// The block is already confirmed but the receipt is not yet queryable
				// (e.g. Frontier tx-hash mapping lag) or the transaction was reorged out.
				// Return an error so the caller keeps the event queued and retries it.
				return Err(eyre::eyre!(
					"transaction receipt not found for RoundUp event ({:?})",
					tx_hash
				));
			},
		};

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
			pending_relays: Mutex::new(HashMap::new()),
			pending_events: Mutex::new(Vec::new()),
		}
	}

	/// Decode & Serialize log to `RoundUp` struct.
	async fn decode_log(&self, log: Log) -> Result<RoundUp> {
		Ok(log.log_decode::<RoundUp>()?.inner.data)
	}

	/// Processes queued RoundUp events whose emitting block has reached
	/// `block_confirmations` on the bifrost chain.
	///
	/// Events that process successfully are removed from the queue. Events that fail
	/// (e.g. a transaction receipt that is not yet queryable) are kept and retried on
	/// the next tick, until they succeed or [`PENDING_EVENT_MAX_AGE`] elapses.
	async fn process_confirmed_pending_events(&self) -> Result<()> {
		let latest_block = self.client.get_block_number().await?;
		let block_confirmations = self.client.metadata.block_confirmations;

		// Snapshot the confirmed events (oldest first) without holding the lock across `.await`.
		let confirmed: Vec<(usize, Log, Duration)> = {
			let pending = self.pending_events.lock().unwrap();
			pending
				.iter()
				.enumerate()
				.filter(|(_, e)| latest_block.saturating_sub(e.block_number) >= block_confirmations)
				.map(|(idx, e)| (idx, e.log.clone(), e.first_seen.elapsed()))
				.collect()
		};
		if confirmed.is_empty() {
			return Ok(());
		}

		// Indices to drop from the queue: successfully processed, or aged out.
		let mut done: Vec<usize> = Vec::new();
		for (idx, log, age) in confirmed {
			match self.process_confirmed_log(&log, false).await {
				Ok(()) => done.push(idx),
				Err(e) if age >= PENDING_EVENT_MAX_AGE => {
					done.push(idx);
					br_primitives::log_and_capture!(
						error,
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						"❗️ Dropping RoundUp event ({:?}) still unprocessable after {:?}: {:?}",
						log.transaction_hash,
						age,
						e
					);
				},
				Err(e) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚠️ Failed to process RoundUp event ({:?}), will retry: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						log.transaction_hash,
						e,
					);
				},
			}
		}

		if !done.is_empty() {
			let mut pending = self.pending_events.lock().unwrap();
			done.sort_unstable_by(|a, b| b.cmp(a));
			for idx in done {
				if idx < pending.len() {
					pending.remove(idx);
				}
			}
		}
		Ok(())
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
		if self.external_clients.is_empty() {
			return Ok(());
		}

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

		Ok(())
	}

	/// Retries all pending roundup relays. Called every 10 minutes via interval tick.
	///
	/// For each external chain with pending relays, fetches its current round and:
	/// - Drops the entire chain entry if the chain is already synced to the bifrost latest round.
	/// - Otherwise relays all pending rounds (ascending) whose round exceeds the chain's current round.
	async fn retry_pending_relays(&self) -> Result<()> {
		let chain_ids: Vec<ChainId> = self.pending_relays.lock().unwrap().keys().cloned().collect();
		if chain_ids.is_empty() {
			return Ok(());
		}

		let bifrost_latest_round =
			self.client.protocol_contracts.authority.latest_round().call().await?;

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
			// When CCCPRelayQueue pallet is absent, SocketQueuePoller is not spawned, so nobody
			// would ever advance past BootstrapSocketRelayQueue. Skip straight to
			// BootstrapSocketRelay so SocketRelayHandler is unblocked immediately.
			let next_state = if self.bootstrap_shared_data.cccp_relay_queue_enabled {
				BootstrapState::BootstrapSocketRelayQueue
			} else {
				BootstrapState::BootstrapSocketRelay
			};
			let mut bootstrap_states = self.bootstrap_shared_data.bootstrap_states.write().await;
			for chain_id in chain_ids {
				*bootstrap_states.get_mut(&chain_id).unwrap() = next_state;
			}
		}

		if self.bootstrap_shared_data.cccp_relay_queue_enabled {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚙️  [Bootstrap mode] BootstrapRoundUpPhase2 → BootstrapSocketRelayQueue",
				sub_display_format(SUB_LOG_TARGET),
			);
		} else {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚙️  [Bootstrap mode] BootstrapRoundUpPhase2 → BootstrapSocketRelay (CCCPRelayQueue absent)",
				sub_display_format(SUB_LOG_TARGET),
			);
		}
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
