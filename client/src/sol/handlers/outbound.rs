// SPDX-License-Identifier: Apache-2.0
//
// Unified Solana outbound handler. One per cluster; owns both halves of
// the BFC ↔ Solana flow:
//
//   1. BFC → Solana: drains the `SolOutboundJob` queue and submits the
//      cccp-solana `poll(...)` / `round_control_relay(...)` IX with the
//      fee-payer keypair (retry + priority-fee escalation). Prepends an
//      idempotent create-ATA IX so first-time recipients don't bounce.
//   2. Solana → BFC commit mirror: the program emits `Executed`/`Reverted`
//      on a `poll(...)` result; this worker relays it to BFC's EVM Socket
//      contract (status as-is, empty sigs).
//
// `is_relay_target = false` (watch-only) disables (1) only; the commit
// mirror in (2) stays active.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy::{
	network::Network,
	primitives::{Address, Bytes, FixedBytes, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use sc_service::SpawnTaskHandle;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::Instruction;
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer, read_keypair_file};
use solana_sdk::transaction::Transaction;
use solana_transaction_status_client_types::TransactionConfirmationStatus;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_stream::{StreamExt, wrappers::UnboundedReceiverStream};

use br_primitives::{
	contracts::socket::Socket_Struct::{
		Instruction as EvmInstruction, RequestID, Signatures as EvmSignatures, Socket_Message,
		Task_Params,
	},
	eth::SocketEventStatus,
	sol::{Event, EventMessage, EventType},
	tx::SocketRelayMetadata,
	utils::sub_display_format,
};

use crate::eth::{EthClient, send_transaction, traits::SocketRelayBuilder};
use crate::sol::ata::{build_create_ata_idempotent_ix, derive_ata};
use crate::sol::client::SolClient;
use crate::sol::codec::{AssetIndex, PollSubmit, RoundUpSubmit, Signatures};
use crate::sol::ix_builder::{
	MAX_ROUND_RELAYER_COUNT, PollTokenAccounts, RELAYERS_PER_CHUNK, SIGS_PER_CHUNK,
	build_close_poll_signatures_ix, build_close_round_ix, build_close_roundup_signatures_ix,
	build_poll_buffered_ix, build_poll_ix, build_round_control_relay_buffered_ix,
	build_round_control_relay_ix, build_submit_roundup_relayers_ix,
	build_submit_roundup_signatures_ix, build_submit_signatures_ix,
};
use crate::sol::pda;
use crate::sol::registry::AssetRegistry;
use crate::sol::slot_manager::EventDelivery;

const SUB_LOG_TARGET: &str = "sol-outbound";

/// How often the handler walks its `pending_jobs` list and retries the
/// ones whose backoff has elapsed. 60s is tight enough to recover from
/// transient RPC blips quickly but loose enough that sustained cluster
/// outages don't burn tx fees. Also caps the re-lock latency on the
/// pending list.
const PENDING_RETRY_TICK: Duration = Duration::from_secs(60);

/// Maximum number of retry attempts before a pending job is dropped.
/// After ~1h of cluster outage (with the default tick + backoff
/// schedule) we give up and log an error so ops can decide whether to
/// manually re-submit.
const MAX_PENDING_ATTEMPTS: u32 = 10;

/// Starting backoff before the first retry. Doubles on every failure
/// up to `MAX_PENDING_BACKOFF`.
const INITIAL_PENDING_BACKOFF: Duration = Duration::from_secs(30);

/// Upper bound on the per-job backoff window.
const MAX_PENDING_BACKOFF: Duration = Duration::from_secs(15 * 60);

fn build_commit_socket_message(ev: &Event) -> Socket_Message {
	Socket_Message {
		req_id: RequestID {
			ChainIndex: FixedBytes::from(ev.req_chain),
			round_id: ev.round_id,
			sequence: ev.sequence,
		},
		status: ev.status,
		ins_code: EvmInstruction {
			ChainIndex: FixedBytes::from(ev.ins_code_chain),
			RBCmethod: FixedBytes::from(ev.ins_code_method),
		},
		params: Task_Params {
			tokenIDX0: FixedBytes::from(ev.asset_index),
			tokenIDX1: FixedBytes::from(ev.token_idx1),
			refund: Address::from(ev.refund),
			to: Address::from(ev.to),
			amount: U256::from_be_bytes(ev.amount),
			variants: Bytes::from(ev.variants.clone()),
		},
	}
}

// ── Compute Budget Program ──────────────────────────────────────────
// We construct the IX data by hand to avoid pulling in an extra crate.
// Program ID: ComputeBudget111111111111111111111111111111
//   base58("ComputeBudget111111111111111111111111111111") → the 32 bytes
//   below. Any drift here surfaces as `ProgramAccountNotFound` at preflight
//   (or `Attempt to load a program that does not exist` at the validator),
//   because the BPF loader can't resolve the phony program account that the
//   wrong bytes encode.
const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
	3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231, 188, 140, 229, 187,
	197, 247, 18, 107, 44, 67, 155, 58, 64, 0, 0, 0,
]);

/// SetComputeUnitLimit(u32) — instruction discriminator 0x02.
fn compute_budget_set_cu_limit(units: u32) -> Instruction {
	let mut data = vec![0x02];
	data.extend_from_slice(&units.to_le_bytes());
	Instruction { program_id: COMPUTE_BUDGET_PROGRAM_ID, accounts: vec![], data }
}

/// SetComputeUnitPrice(u64) — instruction discriminator 0x03.
fn compute_budget_set_cu_price(micro_lamports: u64) -> Instruction {
	let mut data = vec![0x03];
	data.extend_from_slice(&micro_lamports.to_le_bytes());
	Instruction { program_id: COMPUTE_BUDGET_PROGRAM_ID, accounts: vec![], data }
}

// ── Defaults ────────────────────────────────────────────────────────
const DEFAULT_COMPUTE_UNIT_LIMIT: u32 = 400_000;
/// Verifying a 33-of-64 quorum exhausts the default 400k limit before the
/// buffered finalizer reaches settlement. Reserve Solana's transaction-level
/// ceiling for poll and roundup finalizers, whose cost scales with signatures.
const BUFFERED_FINALIZE_COMPUTE_UNIT_LIMIT: u32 = 1_400_000;
const DEFAULT_BASE_PRIORITY_FEE: u64 = 1_000;
const DEFAULT_MAX_PRIORITY_FEE: u64 = 1_000_000;
const DEFAULT_CONFIRMATION_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_SEND_RETRIES: u32 = 3;
const PRIORITY_FEE_MULTIPLIER: u64 = 3;
const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_millis(2_000);
/// Solana's maximum transaction packet size.
const SOLANA_MAX_TX_SIZE: usize = 1232;

// ── Round-rent sweep ────────────────────────────────────────────────
/// How often the handler sweeps expired `RoundInfo` PDAs to reclaim the
/// rent-exempt deposit this relayer funded. Rounds advance slowly (one per
/// BFC round period, ~minutes), so a multi-minute cadence keeps RPC load
/// negligible while still recovering deposits within an hour of expiry.
const ROUND_SWEEP_TICK: Duration = Duration::from_secs(10 * 60);

/// Upper bound on how many rounds a single sweep tick inspects. Caps the
/// `getMultipleAccounts` fan-out per tick; the cursor advances by this much
/// each time so a backlog drains over successive ticks rather than in one
/// burst.
const ROUND_SWEEP_MAX_PER_TICK: u64 = 256;

/// On the first sweep after boot, look back at most this many rounds below
/// the active window rather than scanning from genesis. Pre-feature rounds
/// carry no recorded payer (so are never closable) and re-scanning the full
/// history every restart is wasteful — this bounds the catch-up to a recent
/// window that still recovers self-funded rounds expired during a brief
/// downtime.
const ROUND_SWEEP_BOOT_LOOKBACK: u64 = 1024;

/// Work item pushed onto the outbound queue.
#[derive(Debug, Clone)]
pub enum SolOutboundJob {
	/// Plain `poll(...)` job. The caller has already resolved every
	/// token account address — used by tests / round trips.
	Poll { submit: PollSubmit, asset_index: AssetIndex, tokens: PollTokenAccounts },
	/// `poll(...)` job that lets the handler resolve the token accounts
	/// from its `AssetRegistry` + the supplied recipient Solana wallet.
	/// This is the variant the Bifrost socket relay handler dispatches
	/// from `dispatch_to_solana`.
	PollFromBfc {
		submit: PollSubmit,
		asset_index: AssetIndex,
		/// Solana wallet that owns the recipient ATA. The relayer derives
		/// the actual ATA address inside `handle()`.
		recipient_wallet: Pubkey,
	},
	/// `round_control_relay(...)` for relayer set rotation.
	RoundControlRelay { submit: RoundUpSubmit },
}

/// Channel used by upstream workers (Bifrost socket relay handler,
/// round-up emitter) to push work onto the outbound submission queue.
pub type SolOutboundSender = UnboundedSender<SolOutboundJob>;

/// One entry in the retry backlog. The `next_retry_at` deadline is an
/// in-memory `Instant`, so restarting the handler starts the backoff
/// clock fresh — that's acceptable because restarting already implies
/// ops intent to replay.
struct PendingJob {
	job: SolOutboundJob,
	attempts: u32,
	next_retry_at: tokio::time::Instant,
}

/// Unified Solana outbound worker. One per Solana cluster. The generics
/// track the Bifrost `EthClient` the commit mirror relays through.
pub struct SolOutboundHandler<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub client: SolClient,
	rpc: Arc<RpcClient>,
	receiver: UnboundedReceiver<SolOutboundJob>,
	/// Loaded fee-payer Ed25519 keypair (= `solana_sdk::Keypair`).
	fee_payer: Keypair,
	/// Asset registry — maps CCCP asset indexes to SPL mints and
	/// pre-derived vault ATAs. Built from static config at boot.
	pub registry: AssetRegistry,
	/// Priority fee config (micro-lamports per CU).
	base_priority_fee: u64,
	max_priority_fee: u64,
	confirmation_timeout: Duration,
	max_send_retries: u32,
	/// Jobs that failed `handle()` and are waiting for a retry. Only
	/// `Poll` / `PollFromBfc` enter this list — `RoundControlRelay`
	/// failures are recovered by the upstream `RoundupRelayHandler`'s
	/// own pending queue, which keys on `submit.round` and re-dispatches
	/// on its own 10-minute tick.
	pending: Vec<PendingJob>,
	// ---- commit-mirror state ----
	/// Bifrost client — relays the commit-mirror `poll(...)` to BFC's Socket.
	bfc_client: Arc<EthClient<F, P, N>>,
	/// Spawn handle + debug flag for the commit-mirror `send_transaction`.
	handle: SpawnTaskHandle,
	debug_mode: bool,
	/// Lossless per-consumer slot-manager queue (`EventType::Outbound`).
	event_stream: UnboundedReceiverStream<EventDelivery>,
}

impl<F, P, N: Network> SocketRelayBuilder<F, P, N> for SolOutboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn get_client(&self) -> Arc<EthClient<F, P, N>> {
		self.bfc_client.clone()
	}
}

impl<F, P, N: Network> SolOutboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// Create a new handler. Returns the `(handler, sender)` pair so the
	/// service-deps wiring can hand the sender to upstream workers.
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		client: SolClient,
		fee_payer_keypair_path: PathBuf,
		registry: AssetRegistry,
		base_priority_fee: Option<u64>,
		max_priority_fee: Option<u64>,
		confirmation_timeout_secs: Option<u64>,
		max_send_retries: Option<u32>,
		bfc_client: Arc<EthClient<F, P, N>>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
		event_receiver: UnboundedReceiver<EventDelivery>,
	) -> eyre::Result<(Self, SolOutboundSender)> {
		let fee_payer = read_keypair_file(&fee_payer_keypair_path).map_err(|err| {
			eyre::eyre!(
				"failed to load Solana fee-payer keypair from {}: {}",
				fee_payer_keypair_path.display(),
				err
			)
		})?;
		let rpc = client.rpc();
		let (sender, receiver) = mpsc::unbounded_channel();

		Ok((
			Self {
				client,
				rpc,
				receiver,
				fee_payer,
				registry,
				base_priority_fee: base_priority_fee.unwrap_or(DEFAULT_BASE_PRIORITY_FEE),
				max_priority_fee: max_priority_fee.unwrap_or(DEFAULT_MAX_PRIORITY_FEE),
				confirmation_timeout: Duration::from_secs(
					confirmation_timeout_secs.unwrap_or(DEFAULT_CONFIRMATION_TIMEOUT_SECS),
				),
				max_send_retries: max_send_retries.unwrap_or(DEFAULT_MAX_SEND_RETRIES),
				pending: Vec::new(),
				bfc_client,
				handle,
				debug_mode,
				event_stream: UnboundedReceiverStream::new(event_receiver),
			},
			sender,
		))
	}

	/// Schedule a failed `Poll` / `PollFromBfc` job for retry. Uses
	/// exponential backoff capped at `MAX_PENDING_BACKOFF`. Jobs that
	/// have exhausted `MAX_PENDING_ATTEMPTS` are dropped and logged.
	fn schedule_retry(&mut self, job: SolOutboundJob, attempts: u32) {
		if attempts >= MAX_PENDING_ATTEMPTS {
			log::error!(
				target: &self.client.get_chain_name(),
				"[{}] dropping outbound job after {} attempts",
				SUB_LOG_TARGET,
				attempts,
			);
			br_metrics::increase_sol_poll_submissions(&self.client.name, "dropped");
			br_metrics::set_sol_pending_relays(&self.client.name, self.pending.len() as u64);
			return;
		}

		// backoff = INITIAL * 2^attempts, capped at MAX_PENDING_BACKOFF.
		let shift = attempts.min(16); // avoid overflow; 2^16 = 65536s >> cap
		let backoff =
			INITIAL_PENDING_BACKOFF.saturating_mul(1u32 << shift).min(MAX_PENDING_BACKOFF);
		let next_retry_at = tokio::time::Instant::now() + backoff;

		self.pending.push(PendingJob { job, attempts: attempts + 1, next_retry_at });
		br_metrics::set_sol_pending_relays(&self.client.name, self.pending.len() as u64);
	}

	/// Walk `pending` and retry every job whose backoff has elapsed.
	/// Successful retries are dropped from the list; repeat failures
	/// roll the attempts counter forward via `schedule_retry`.
	async fn drain_pending(&mut self) {
		if self.pending.is_empty() {
			return;
		}

		let now = tokio::time::Instant::now();
		// Split `ready` out of `pending` without holding a borrow across
		// the `.await` below.
		let (ready, stalled): (Vec<PendingJob>, Vec<PendingJob>) =
			std::mem::take(&mut self.pending)
				.into_iter()
				.partition(|p| p.next_retry_at <= now);
		self.pending = stalled;
		br_metrics::set_sol_pending_relays(&self.client.name, self.pending.len() as u64);

		for PendingJob { job, attempts, .. } in ready {
			match self.handle(job.clone()).await {
				Ok(()) => log::info!(
					target: &self.client.get_chain_name(),
					"[{}] pending job succeeded on attempt {attempts}",
					SUB_LOG_TARGET,
				),
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"[{}] pending retry {attempts} failed: {err:?}",
						SUB_LOG_TARGET,
					);
					self.schedule_retry(job, attempts);
				},
			}
		}
	}

	/// Borrow the fee-payer pubkey, useful for diagnostics.
	pub fn fee_payer_pubkey(&self) -> Pubkey {
		self.fee_payer.pubkey()
	}

	/// Run the worker loop. Pops jobs off the channel and submits them
	/// one at a time. We deliberately do NOT batch multiple jobs into a
	/// single transaction — Bifrost's relayer protocol expects each
	/// `poll` / `round_control_relay` to land in its own tx so failures
	/// are isolated and so the on-chain `init_if_needed` PDA allocation
	/// for `RequestRecord` is per-message.
	pub async fn run(&mut self) -> eyre::Result<()> {
		let relay_target = self.client.is_relay_target;
		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] outbound handler started; fee_payer={} assets={} relay_target={}",
			SUB_LOG_TARGET,
			self.fee_payer_pubkey(),
			self.registry.len(),
			relay_target,
		);

		// Watch-only clusters skip the BFC → Solana submission loop
		// entirely (no jobs drained, no retry ticker) but still drive
		// the Solana → BFC commit mirror so observed `Accepted`/`Rejected`
		// events round-trip back to BFC.
		if !relay_target {
			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] submission path idle (watch-only); commit mirror active",
				SUB_LOG_TARGET,
			);
			loop {
				tokio::select! {
					item = self.event_stream.next() => {
						match item {
							None => return Ok(()),
							Some(delivery) => {
								self.handle_commit_event_message(&delivery.message).await;
								delivery.acknowledge();
							},
						}
					},
					maybe_job = self.receiver.recv() => {
						match maybe_job {
							None => return Ok(()),
							Some(job) => log::debug!(
								target: &self.client.get_chain_name(),
								"[{}] dropping outbound job on watch-only cluster: {job:?}",
								SUB_LOG_TARGET,
							),
						}
					},
				}
			}
		}

		let mut retry_ticker = tokio::time::interval(PENDING_RETRY_TICK);
		// First tick fires immediately; skip it so we don't retry before
		// any failure has occurred.
		retry_ticker.tick().await;

		let mut sweep_ticker = tokio::time::interval(ROUND_SWEEP_TICK);
		// First tick fires immediately; skip it so the sweep cursor is
		// seeded one full interval after boot (gives the cluster time to
		// report a stable round window).
		sweep_ticker.tick().await;
		// Next round id this relayer has not yet considered for closing.
		// `None` until the first sweep seeds it from the live round window.
		let mut sweep_cursor: Option<u64> = None;

		loop {
			tokio::select! {
				item = self.event_stream.next() => {
					match item {
						None => {
							// Slot manager dropped the broadcast — finish in-flight
							// retries and exit so the supervisor can restart.
							self.drain_pending().await;
							return Ok(());
						}
						Some(delivery) => {
							self.handle_commit_event_message(&delivery.message).await;
							delivery.acknowledge();
						},
					}
				},
				maybe_job = self.receiver.recv() => {
					let Some(job) = maybe_job else {
						// Sender dropped — all upstream workers have shut down.
						// Drain any stragglers before returning so a bounded
						// number of in-flight retries still complete.
						self.drain_pending().await;
						return Ok(());
					};
					let is_roundup = matches!(job, SolOutboundJob::RoundControlRelay { .. });
					match self.handle(job.clone()).await {
						Ok(()) => {},
						Err(err) => {
							if is_roundup {
								// RoundControlRelay is retried by the upstream
								// RoundupRelayHandler's pending queue keyed on
								// `submit.round` — don't double-retry here.
								log::warn!(
									target: &self.client.get_chain_name(),
									"[{}] round_control_relay failed (upstream will retry): {err:?}",
									SUB_LOG_TARGET,
								);
							} else {
								log::warn!(
									target: &self.client.get_chain_name(),
									"[{}] poll job failed (scheduled for retry): {err:?}",
									SUB_LOG_TARGET,
								);
								self.schedule_retry(job, 0);
							}
						},
					}
				},
				_ = retry_ticker.tick() => {
					self.drain_pending().await;
				},
				_ = sweep_ticker.tick() => {
					if let Err(err) = self.sweep_expired_rounds(&mut sweep_cursor).await {
						log::warn!(
							target: &self.client.get_chain_name(),
							"[{}] round-rent sweep tick failed: {err:?}",
							SUB_LOG_TARGET,
						);
					}
				},
			}
		}
	}

	/// Reclaim the rent-exempt deposit of expired `RoundInfo` PDAs this
	/// relayer funded. Walks a bounded window of rounds that have dropped
	/// out of the active set, fetches each round's recorded `payer`, and
	/// sends `close_round` for the ones funded by this relayer's fee-payer.
	///
	/// The on-chain `close_round` IX independently re-checks both the payer
	/// match and the active-window guard, so a stale local view here can
	/// only ever cause a harmless rejected tx — never an erroneous close.
	async fn sweep_expired_rounds(&self, cursor: &mut Option<u64>) -> eyre::Result<()> {
		let (latest, active) = self.client.round_window().await?;

		// Highest round that is strictly outside the active window, i.e.
		// `round_id + active_rounds_size < latest_round_id`. Below 1 means
		// nothing is closable yet.
		let hi = match latest.checked_sub(active).and_then(|v| v.checked_sub(1)) {
			Some(h) => h,
			None => return Ok(()), // active window still covers every round
		};

		// Seed the cursor on the first sweep: start a bounded look-back
		// below the current closable edge rather than from genesis.
		let lo = match *cursor {
			Some(c) => c,
			None => {
				let seed = hi.saturating_sub(ROUND_SWEEP_BOOT_LOOKBACK);
				*cursor = Some(seed);
				seed
			},
		};

		if lo > hi {
			return Ok(()); // caught up — nothing newly expired
		}

		// Bound the per-tick scan; advance the cursor by what we cover.
		let scan_hi = hi.min(lo + ROUND_SWEEP_MAX_PER_TICK - 1);
		let round_ids: Vec<u64> = (lo..=scan_hi).collect();

		let me = self.fee_payer.pubkey();
		let payers = self.client.round_info_payers(&round_ids).await?;

		let mine: Vec<u64> = round_ids
			.iter()
			.zip(payers.iter())
			.filter_map(|(rid, payer)| match payer {
				Some(p) if *p == me => Some(*rid),
				_ => None,
			})
			.collect();

		if !mine.is_empty() {
			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] round-rent sweep: reclaiming {} expired round(s) in [{lo}, {scan_hi}] (latest={latest} active={active})",
				SUB_LOG_TARGET,
				mine.len(),
			);
		}

		for rid in mine {
			let ix = build_close_round_ix(&self.client.program_id, &me, rid);
			match self.send_single_ix(ix).await {
				Ok(()) => log::debug!(
					target: &self.client.get_chain_name(),
					"[{}] closed expired round {rid}, rent refunded to {me}",
					SUB_LOG_TARGET,
				),
				// Best-effort: a failed close just leaves the deposit locked
				// until a later tick retries (the cursor still advances, but
				// a subsequent boot's look-back or a manual sweep recovers
				// it). Don't abort the whole tick on one bad round.
				Err(err) => log::warn!(
					target: &self.client.get_chain_name(),
					"[{}] close_round({rid}) failed: {err:?}",
					SUB_LOG_TARGET,
				),
			}
		}

		// Advance past the window we just scanned.
		*cursor = Some(scan_hi + 1);
		Ok(())
	}

	// ---------------------------------------------------------------------
	// Solana → BFC commit mirror
	// ---------------------------------------------------------------------

	async fn handle_commit_event_message(&self, msg: &EventMessage) {
		if msg.event_type != EventType::Outbound {
			return;
		}
		for ev in &msg.events {
			self.handle_commit_event(ev).await;
		}
	}

	/// Relay a terminal outbound result (`Executed` / `Reverted`) to BFC's
	/// EVM Socket — status as-is, empty sigs (the EVM `SocketRelayHandler`
	/// convention for a destination execution result).
	async fn handle_commit_event(&self, ev: &Event) {
		let status = SocketEventStatus::from(ev.status);
		if !matches!(status, SocketEventStatus::Executed | SocketEventStatus::Reverted) {
			log::debug!(
				target: &self.client.get_chain_name(),
				"-[{}] skipping non-terminal outbound event status={:?} seq={} sig={}",
				sub_display_format(SUB_LOG_TARGET),
				status,
				ev.sequence,
				ev.signature,
			);
			return;
		}

		// Avoid the RPC round-trip + tx build if we can't participate this
		// round.
		match self.bfc_client.is_selected_relayer().await {
			Ok(true) => {},
			Ok(false) => {
				log::debug!(
					target: &self.client.get_chain_name(),
					"-[{}] not a selected relayer this round; skipping seq={} sig={}",
					sub_display_format(SUB_LOG_TARGET),
					ev.sequence,
					ev.signature,
				);
				return;
			},
			Err(err) => {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] is_selected_relayer rpc failed: {err}",
					sub_display_format(SUB_LOG_TARGET),
				);
				return;
			},
		}

		let socket_msg = build_commit_socket_message(ev);
		let tx_request = self.build_poll_request(socket_msg, EvmSignatures::default());

		// src = BFC, dst = this Solana cluster; is_inbound = false.
		let metadata = SocketRelayMetadata::new(
			false,
			status,
			ev.sequence,
			self.bfc_client.chain_id(),
			self.client.chain_id,
			Address::from(ev.to),
			false,
		);

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 🔖 relaying sol outbound-commit to BFC Socket: {} sig={}",
			sub_display_format(SUB_LOG_TARGET),
			metadata,
			ev.signature,
		);

		send_transaction(
			self.bfc_client.clone(),
			tx_request,
			format!("{} ({})", SUB_LOG_TARGET, self.client.get_chain_name()),
			Arc::new(metadata),
			self.debug_mode,
			self.handle.clone(),
		);
	}

	async fn handle(&self, job: SolOutboundJob) -> eyre::Result<()> {
		// Check if we need the buffered path (too many sigs for a single tx).
		let needs_buffer = match &job {
			SolOutboundJob::Poll { submit, .. } | SolOutboundJob::PollFromBfc { submit, .. } => {
				submit.sigs.r.len() > SIGS_PER_CHUNK
			},
			_ => false,
		};

		if needs_buffer {
			return self.handle_buffered(job).await;
		}
		// Always use the fully buffered roundup path. Even a small signature
		// set can accompany 64 new EVM addresses (1280 bytes before any
		// transaction framing), so signature count alone is not a safe switch.
		if let SolOutboundJob::RoundControlRelay { submit } = &job {
			return self.handle_roundup_buffered(submit).await;
		}

		let app_ixs = match &job {
			SolOutboundJob::Poll { submit, asset_index, tokens } => vec![build_poll_ix(
				&self.client.program_id,
				&self.fee_payer.pubkey(),
				submit,
				asset_index,
				tokens,
			)],
			SolOutboundJob::PollFromBfc { submit, asset_index, recipient_wallet } => {
				self.build_poll_ix_from_bfc(submit, asset_index, recipient_wallet)?
			},
			SolOutboundJob::RoundControlRelay { submit } => {
				vec![build_round_control_relay_ix(
					&self.client.program_id,
					&self.fee_payer.pubkey(),
					submit,
				)]
			},
		};

		let mut priority_fee = self.base_priority_fee;

		for attempt in 0..=self.max_send_retries {
			// Prepend compute-budget instructions with the current priority fee.
			let mut ixs = vec![
				compute_budget_set_cu_limit(DEFAULT_COMPUTE_UNIT_LIMIT),
				compute_budget_set_cu_price(priority_fee),
			];
			ixs.extend(app_ixs.clone());

			let recent_blockhash = self
				.rpc
				.get_latest_blockhash()
				.await
				.map_err(|e| eyre::eyre!("get_latest_blockhash: {e}"))?;

			// Pre-flight: compile the message and check serialized size.
			// Solana's max tx packet size is 1232 bytes. We estimate the
			// wire size as: compact(num_sigs) + num_sigs*64 + message bytes.
			let message = Message::new(&ixs, Some(&self.fee_payer.pubkey()));
			let message_data = message.serialize();
			let num_signers = message.header.num_required_signatures as usize;
			// compact-u16 encoding for 1 signer = 1 byte
			let tx_size = 1 + num_signers * 64 + message_data.len();
			if tx_size > SOLANA_MAX_TX_SIZE {
				return Err(eyre::eyre!(
					"transaction exceeds Solana's {SOLANA_MAX_TX_SIZE}B limit \
                     (actual: {tx_size}B). Reduce CCCP signature count or \
                     wait for on-chain signature buffer support."
				));
			}
			log::debug!(
				target: &self.client.get_chain_name(),
				"[{}] tx size: {tx_size}/{SOLANA_MAX_TX_SIZE} bytes",
				SUB_LOG_TARGET,
			);

			let tx = Transaction::new(&[&self.fee_payer], message, recent_blockhash);

			// Send without waiting for confirmation — we'll poll manually.
			let signature = self
				.rpc
				.send_transaction(&tx)
				.await
				.map_err(|e| eyre::eyre!("send_transaction: {e}"))?;
			br_metrics::increase_sol_poll_submissions(&self.client.name, "sent");

			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] tx sent: sig={signature} priority_fee={priority_fee} attempt={}",
				SUB_LOG_TARGET,
				attempt + 1,
			);

			match self.await_confirmation(&signature).await {
				Ok(()) => {
					log::info!(
						target: &self.client.get_chain_name(),
						"[{}] tx confirmed: sig={signature}",
						SUB_LOG_TARGET,
					);
					br_metrics::increase_sol_poll_submissions(&self.client.name, "confirmed");
					return Ok(());
				},
				Err(err) => {
					if attempt < self.max_send_retries {
						let next_fee = priority_fee
							.saturating_mul(PRIORITY_FEE_MULTIPLIER)
							.min(self.max_priority_fee);
						log::warn!(
							target: &self.client.get_chain_name(),
							"[{}] confirmation timeout (attempt {}): {err}; \
							 escalating priority fee {priority_fee} → {next_fee}",
							SUB_LOG_TARGET,
							attempt + 1,
						);
						priority_fee = next_fee;
					} else {
						br_metrics::increase_sol_poll_submissions(&self.client.name, "failed");
						return Err(eyre::eyre!(
							"tx failed after {} attempts (last sig={signature}): {err}",
							self.max_send_retries + 1
						));
					}
				},
			}
		}

		unreachable!()
	}

	/// Poll `getSignatureStatuses` until the transaction lands or the
	/// timeout expires. Returns `Ok(())` on confirmation, `Err` on
	/// timeout or terminal failure (e.g. InstructionError).
	async fn await_confirmation(&self, signature: &Signature) -> eyre::Result<()> {
		let deadline = tokio::time::Instant::now() + self.confirmation_timeout;

		loop {
			if tokio::time::Instant::now() >= deadline {
				return Err(eyre::eyre!(
					"confirmation timeout after {:?}",
					self.confirmation_timeout
				));
			}

			tokio::time::sleep(CONFIRMATION_POLL_INTERVAL).await;

			let statuses = self
				.rpc
				.get_signature_statuses(&[*signature])
				.await
				.map_err(|e| eyre::eyre!("get_signature_statuses: {e}"))?;

			if let Some(Some(status)) = statuses.value.first() {
				if let Some(ref err) = status.err {
					return Err(eyre::eyre!("tx failed on-chain: {err:?}"));
				}
				if status.confirmation_status == Some(TransactionConfirmationStatus::Finalized) {
					return Ok(());
				}
			}
			// `None` → not yet seen by the cluster, keep polling.
		}
	}

	/// Buffered path: split signatures into chunks, submit each chunk
	/// as a separate `submit_signatures` tx, then send `poll_buffered`.
	async fn handle_buffered(&self, job: SolOutboundJob) -> eyre::Result<()> {
		// `Some(wallet)` = build a `create_associated_token_account` IX so
		// the recipient ATA is materialized on first use (`PollFromBfc`
		// path). `None` = the caller already produced the ATA in
		// `tokens.recipient_token_account` and cannot tell us the owning
		// wallet (`Poll` path, used by tests/round-trips) — in that case we
		// skip create_ata and rely on the ATA existing already.
		let (submit, asset_index, tokens, recipient_wallet) = match &job {
			SolOutboundJob::PollFromBfc { submit, asset_index, recipient_wallet } => {
				let entry = self.registry.get(&asset_index.0).ok_or_else(|| {
					eyre::eyre!(
						"no asset registry entry for index 0x{}",
						hex::encode(asset_index.0)
					)
				})?;
				let recipient_ata = derive_ata(recipient_wallet, &entry.mint);
				let tokens = PollTokenAccounts {
					mint: entry.mint,
					vault_token_account: entry.vault_token_account,
					recipient_token_account: recipient_ata,
				};
				(submit, asset_index, tokens, Some(*recipient_wallet))
			},
			SolOutboundJob::Poll { submit, asset_index, tokens } => {
				(submit, asset_index, *tokens, None)
			},
			_ => return Err(eyre::eyre!("handle_buffered called for non-poll job")),
		};

		let req_id_pack = submit.msg.req_id.pack();
		let total_sigs = submit.sigs.r.len();
		let fee_payer = self.fee_payer.pubkey();

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] using buffered path for {total_sigs} signatures",
			SUB_LOG_TARGET,
		);

		// A prior attempt may have finalized only some chunks before an RPC
		// timeout or process-level retry. Appending the full bundle again
		// would duplicate signatures and permanently poison the buffer, so
		// every attempt starts from a clean payer-scoped PDA.
		let (poll_sigs_pda, _) = pda::poll_signatures_raw(
			&self.client.program_id,
			&req_id_pack,
			submit.msg.status,
			&fee_payer,
		);
		let existing = self
			.rpc
			.get_account_with_commitment(
				&poll_sigs_pda,
				solana_commitment_config::CommitmentConfig::finalized(),
			)
			.await
			.map_err(|e| eyre::eyre!("probe signature buffer {poll_sigs_pda}: {e}"))?;
		if existing.value.is_some() {
			self.send_single_ix(build_close_poll_signatures_ix(
				&self.client.program_id,
				&fee_payer,
				&req_id_pack,
				submit.msg.status,
			))
			.await
			.map_err(|e| eyre::eyre!("reset stale signature buffer {poll_sigs_pda}: {e}"))?;
		}

		// Phase 1: submit signature chunks
		for chunk_start in (0..total_sigs).step_by(SIGS_PER_CHUNK) {
			let chunk_end = (chunk_start + SIGS_PER_CHUNK).min(total_sigs);
			let chunk = Signatures {
				r: submit.sigs.r[chunk_start..chunk_end].to_vec(),
				s: submit.sigs.s[chunk_start..chunk_end].to_vec(),
				v: submit.sigs.v[chunk_start..chunk_end].to_vec(),
			};

			let ix = build_submit_signatures_ix(
				&self.client.program_id,
				&fee_payer,
				&chunk,
				&req_id_pack,
				submit.msg.status,
			);

			self.send_single_ix(ix).await.map_err(|e| {
				eyre::eyre!("submit_signatures chunk {chunk_start}..{chunk_end} failed: {e}")
			})?;

			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] submitted sig chunk {chunk_start}..{chunk_end}/{total_sigs}",
				SUB_LOG_TARGET,
			);
		}

		// Phase 2: send poll_buffered (reads sigs from PDA, no sigs in IX data).
		//
		// create_ata is prepended only when we know the recipient **wallet**
		// (PollFromBfc path). Passing the ATA address as `owner` to
		// `build_create_ata_idempotent_ix` is a bug — it derives a second,
		// unrelated ATA keyed on `(ATA, mint)` instead of the actual
		// recipient's wallet, and the resulting tx fails because the
		// `recipient_token_account` slot of `poll_buffered` still points at
		// the real ATA which may not exist yet.
		let poll_buf = build_poll_buffered_ix(
			&self.client.program_id,
			&fee_payer,
			&submit.msg,
			&submit.option,
			asset_index,
			&tokens,
		);

		let mut ixs = vec![
			compute_budget_set_cu_limit(BUFFERED_FINALIZE_COMPUTE_UNIT_LIMIT),
			compute_budget_set_cu_price(self.base_priority_fee),
		];
		if let Some(wallet) = recipient_wallet {
			ixs.push(build_create_ata_idempotent_ix(&fee_payer, &wallet, &tokens.mint));
		}
		ixs.push(poll_buf);

		let recent_blockhash = self
			.rpc
			.get_latest_blockhash()
			.await
			.map_err(|e| eyre::eyre!("get_latest_blockhash: {e}"))?;

		let message = Message::new(&ixs, Some(&self.fee_payer.pubkey()));
		let tx = Transaction::new(&[&self.fee_payer], message, recent_blockhash);

		let signature = self
			.rpc
			.send_transaction(&tx)
			.await
			.map_err(|e| eyre::eyre!("send poll_buffered: {e}"))?;

		self.await_confirmation(&signature).await?;

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] poll_buffered confirmed: sig={signature}",
			SUB_LOG_TARGET,
		);

		Ok(())
	}

	/// Buffered relayer-set rotation. This mirrors `handle_buffered` but
	/// uses the dedicated roundup PDA and finalizer instructions.
	async fn handle_roundup_buffered(&self, submit: &RoundUpSubmit) -> eyre::Result<()> {
		if submit.sigs.r.len() != submit.sigs.s.len() || submit.sigs.s.len() != submit.sigs.v.len()
		{
			return Err(eyre::eyre!("roundup signature component lengths differ"));
		}
		if submit.new_relayers.is_empty() || submit.new_relayers.len() > MAX_ROUND_RELAYER_COUNT {
			return Err(eyre::eyre!(
				"roundup relayer count must be 1..={MAX_ROUND_RELAYER_COUNT}, got {}",
				submit.new_relayers.len()
			));
		}
		let mut round_bytes = [0u8; 8];
		round_bytes.copy_from_slice(&submit.round[24..32]);
		let round_id = u64::from_be_bytes(round_bytes);
		let fee_payer = self.fee_payer.pubkey();
		let (buffer_pda, _) =
			pda::roundup_signatures(&self.client.program_id, round_id, &fee_payer);

		let existing = self
			.rpc
			.get_account_with_commitment(
				&buffer_pda,
				solana_commitment_config::CommitmentConfig::finalized(),
			)
			.await
			.map_err(|e| eyre::eyre!("probe roundup buffer {buffer_pda}: {e}"))?;
		if existing.value.is_some() {
			self.send_single_ix(build_close_roundup_signatures_ix(
				&self.client.program_id,
				&fee_payer,
				round_id,
			))
			.await
			.map_err(|e| eyre::eyre!("reset stale roundup buffer {buffer_pda}: {e}"))?;
		}

		for chunk_start in (0..submit.sigs.r.len()).step_by(SIGS_PER_CHUNK) {
			let chunk_end = (chunk_start + SIGS_PER_CHUNK).min(submit.sigs.r.len());
			let chunk = Signatures {
				r: submit.sigs.r[chunk_start..chunk_end].to_vec(),
				s: submit.sigs.s[chunk_start..chunk_end].to_vec(),
				v: submit.sigs.v[chunk_start..chunk_end].to_vec(),
			};
			self.send_single_ix(build_submit_roundup_signatures_ix(
				&self.client.program_id,
				&fee_payer,
				&chunk,
				round_id,
			))
			.await
			.map_err(|e| {
				eyre::eyre!(
					"submit_roundup_signatures chunk {chunk_start}..{chunk_end} failed: {e}"
				)
			})?;
		}

		for chunk_start in (0..submit.new_relayers.len()).step_by(RELAYERS_PER_CHUNK) {
			let chunk_end = (chunk_start + RELAYERS_PER_CHUNK).min(submit.new_relayers.len());
			self.send_single_ix(build_submit_roundup_relayers_ix(
				&self.client.program_id,
				&fee_payer,
				&submit.new_relayers[chunk_start..chunk_end],
				round_id,
			))
			.await
			.map_err(|e| {
				eyre::eyre!("submit_roundup_relayers chunk {chunk_start}..{chunk_end} failed: {e}")
			})?;
		}

		self.send_single_ix_with_limit(
			build_round_control_relay_buffered_ix(&self.client.program_id, &fee_payer, submit),
			BUFFERED_FINALIZE_COMPUTE_UNIT_LIMIT,
		)
		.await
		.map_err(|e| eyre::eyre!("round_control_relay_buffered({round_id}) failed: {e}"))
	}

	/// Send a single instruction as a transaction, wait for confirmation.
	async fn send_single_ix(&self, ix: Instruction) -> eyre::Result<()> {
		self.send_single_ix_with_limit(ix, DEFAULT_COMPUTE_UNIT_LIMIT).await
	}

	/// Send a single instruction with an explicit compute limit. Buffered
	/// finalizers need a larger budget because EVM recovery scales with quorum.
	async fn send_single_ix_with_limit(
		&self,
		ix: Instruction,
		compute_unit_limit: u32,
	) -> eyre::Result<()> {
		let ixs = vec![
			compute_budget_set_cu_limit(compute_unit_limit),
			compute_budget_set_cu_price(self.base_priority_fee),
			ix,
		];

		let message = Message::new(&ixs, Some(&self.fee_payer.pubkey()));
		let message_data = message.serialize();
		let tx_size = 1 + message.header.num_required_signatures as usize * 64 + message_data.len();
		if tx_size > SOLANA_MAX_TX_SIZE {
			return Err(eyre::eyre!(
				"transaction exceeds Solana's {SOLANA_MAX_TX_SIZE}B limit (actual: {tx_size}B)"
			));
		}

		let recent_blockhash = self
			.rpc
			.get_latest_blockhash()
			.await
			.map_err(|e| eyre::eyre!("get_latest_blockhash: {e}"))?;

		let tx = Transaction::new(&[&self.fee_payer], message, recent_blockhash);

		let signature = self
			.rpc
			.send_transaction(&tx)
			.await
			.map_err(|e| eyre::eyre!("send_transaction: {e}"))?;

		self.await_confirmation(&signature).await
	}

	/// Build the IX list for a `PollFromBfc` job. Resolves the token
	/// accounts from the asset registry + the supplied recipient wallet,
	/// and prepends a `create_associated_token_account` idempotent IX so
	/// the recipient ATA is materialized if it doesn't already exist.
	fn build_poll_ix_from_bfc(
		&self,
		submit: &PollSubmit,
		asset_index: &AssetIndex,
		recipient_wallet: &Pubkey,
	) -> eyre::Result<Vec<Instruction>> {
		let entry = self.registry.get(&asset_index.0).ok_or_else(|| {
			eyre::eyre!(
				"no asset registry entry for index 0x{}; configure SolProvider.assets",
				hex::encode(asset_index.0)
			)
		})?;

		let recipient_ata = derive_ata(recipient_wallet, &entry.mint);

		let tokens = PollTokenAccounts {
			mint: entry.mint,
			vault_token_account: entry.vault_token_account,
			recipient_token_account: recipient_ata,
		};

		let create_ata =
			build_create_ata_idempotent_ix(&self.fee_payer.pubkey(), recipient_wallet, &entry.mint);

		let poll = build_poll_ix(
			&self.client.program_id,
			&self.fee_payer.pubkey(),
			submit,
			asset_index,
			&tokens,
		);

		Ok(vec![create_ata, poll])
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use br_primitives::cli::SolAssetEntry;

	use crate::sol::{
		codec::{
			AssetIndex, ChainIndex, Instruction as IxCode, RBCmethod, RequestId, Signatures,
			SocketMessage, TaskParams,
		},
		pda,
	};

	fn fake_program_id() -> Pubkey {
		Pubkey::new_unique()
	}

	fn fake_registry(program_id: &Pubkey) -> AssetRegistry {
		let (vault_pda, _) = pda::vault_config(program_id);
		AssetRegistry::from_entries(
			&[SolAssetEntry {
				index: "0x0100010000000000000000000000000000000000000000000000000000000008".into(),
				mint: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".into(),
				name: Some("usdc".into()),
				decimals: Some(6),
			}],
			&vault_pda,
		)
		.unwrap()
	}

	fn sample_submit() -> PollSubmit {
		PollSubmit {
			msg: SocketMessage {
				req_id: RequestId {
					chain: ChainIndex([0, 0, 0x0b, 0xfc]),
					round_id: 1,
					sequence: 7,
				},
				status: 5,
				ins_code: IxCode {
					chain: ChainIndex(*b"SOL\0"),
					method: RBCmethod([
						0x02, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
						0x00, 0x00, 0x00, 0x00,
					]),
				},
				params: TaskParams {
					token_idx0: AssetIndex([
						0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
						0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
						0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,
					]),
					token_idx1: AssetIndex::ZERO,
					refund: [0xab; 20],
					to: [0xcd; 20],
					amount: {
						let mut a = [0u8; 32];
						a[24..32].copy_from_slice(&500_000u64.to_be_bytes());
						a
					},
					variants: vec![],
				},
			},
			sigs: Signatures::default(),
			option: [0u8; 32],
		}
	}

	/// Replicates `build_poll_ix_from_bfc` without instantiating an
	/// actual `SolOutboundHandler` (avoids needing a real keypair file).
	fn build_poll_ix_from_bfc_pure(
		program_id: &Pubkey,
		fee_payer: &Pubkey,
		registry: &AssetRegistry,
		submit: &PollSubmit,
		asset_index: &AssetIndex,
		recipient_wallet: &Pubkey,
	) -> eyre::Result<Vec<Instruction>> {
		let entry = registry
			.get(&asset_index.0)
			.expect("test fixture: asset_index must be in the fake registry");
		let recipient_ata = derive_ata(recipient_wallet, &entry.mint);
		let tokens = PollTokenAccounts {
			mint: entry.mint,
			vault_token_account: entry.vault_token_account,
			recipient_token_account: recipient_ata,
		};
		let create_ata = build_create_ata_idempotent_ix(fee_payer, recipient_wallet, &entry.mint);
		let poll = build_poll_ix(program_id, fee_payer, submit, asset_index, &tokens);
		Ok(vec![create_ata, poll])
	}

	#[test]
	fn poll_from_bfc_prepends_create_ata_and_uses_registry_vault_ata() {
		let program_id = fake_program_id();
		let registry = fake_registry(&program_id);
		let fee_payer = Pubkey::new_unique();
		let recipient_wallet = Pubkey::new_unique();
		let submit = sample_submit();
		let asset_index = AssetIndex([
			0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
			0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
			0x00, 0x00, 0x00, 0x08,
		]);

		let ixs = build_poll_ix_from_bfc_pure(
			&program_id,
			&fee_payer,
			&registry,
			&submit,
			&asset_index,
			&recipient_wallet,
		)
		.unwrap();

		assert_eq!(ixs.len(), 2, "expected create_ata + poll");

		// First IX = create_associated_token_account_idempotent
		let create_ata = &ixs[0];
		assert_eq!(create_ata.data, vec![1u8]);
		assert_eq!(create_ata.accounts.len(), 6);
		assert_eq!(create_ata.accounts[0].pubkey, fee_payer);

		// Second IX = the cccp-solana poll(...) call
		let poll = &ixs[1];
		assert_eq!(poll.program_id, program_id);

		// The poll IX must use the registry-derived vault ATA, not some
		// other random pubkey.
		let entry = registry
			.get(&asset_index.0)
			.expect("test fixture: asset_index must be in the fake registry");
		let expected_vault_ta = derive_ata(&pda::vault_config(&program_id).0, &entry.mint);
		assert_eq!(entry.vault_token_account, expected_vault_ta);
		// Position 7 in the Poll Accounts struct is `vault_token_account`
		// (see `crate::sol::ix_builder` layout).
		assert_eq!(poll.accounts[7].pubkey, expected_vault_ta);

		// Position 8 is `recipient_token_account` — must equal the ATA
		// for our recipient_wallet.
		let expected_recipient_ata = derive_ata(&recipient_wallet, &entry.mint);
		assert_eq!(poll.accounts[8].pubkey, expected_recipient_ata);
	}

	#[test]
	fn poll_job_can_be_constructed() {
		let _job = SolOutboundJob::Poll {
			submit: sample_submit(),
			asset_index: AssetIndex([0u8; 32]),
			tokens: PollTokenAccounts {
				mint: Pubkey::new_unique(),
				vault_token_account: Pubkey::new_unique(),
				recipient_token_account: Pubkey::new_unique(),
			},
		};
	}

	#[test]
	fn commit_message_preserves_secondary_asset_index() {
		let token_idx1 = [0x7a; 32];
		let event = Event {
			signature: "test-signature".into(),
			slot: 42,
			req_chain: [0, 0, 0x0b, 0xfc],
			round_id: 1,
			sequence: 7,
			status: 3,
			ins_code_chain: *b"SOL\0",
			ins_code_method: [0; 16],
			asset_index: [0x11; 32],
			token_idx1,
			to: [0x22; 20],
			refund: [0x33; 20],
			amount: [0x44; 32],
			variants: vec![1, 2, 3],
		};

		let msg = build_commit_socket_message(&event);
		assert_eq!(<[u8; 32]>::from(msg.params.tokenIDX1), token_idx1);
	}

	/// Pure helper mirroring `SolOutboundHandler::schedule_retry`'s
	/// backoff calculation so the contract can be asserted without
	/// instantiating the handler (which requires a keypair file).
	fn backoff_for_attempts(attempts: u32) -> Duration {
		let shift = attempts.min(16);
		INITIAL_PENDING_BACKOFF.saturating_mul(1u32 << shift).min(MAX_PENDING_BACKOFF)
	}

	#[test]
	fn retry_backoff_is_exponential_then_capped() {
		// 0 attempts → initial (30s)
		assert_eq!(backoff_for_attempts(0), INITIAL_PENDING_BACKOFF);
		// 1 attempt  → 60s
		assert_eq!(backoff_for_attempts(1), INITIAL_PENDING_BACKOFF * 2);
		// 2 attempts → 120s
		assert_eq!(backoff_for_attempts(2), INITIAL_PENDING_BACKOFF * 4);
		// Far in the future — must cap at MAX_PENDING_BACKOFF (15m).
		assert_eq!(backoff_for_attempts(10), MAX_PENDING_BACKOFF);
		assert_eq!(backoff_for_attempts(100), MAX_PENDING_BACKOFF);
	}

	#[test]
	fn max_pending_attempts_is_finite() {
		// Sanity guard: make sure we can't accidentally set this to 0
		// (which would drop every failure) or something absurd that
		// leaks jobs indefinitely.
		assert!(MAX_PENDING_ATTEMPTS >= 3, "too few retries for production");
		assert!(MAX_PENDING_ATTEMPTS <= 20, "retry horizon is too long");
	}

	/// Regression for the `handle_buffered` ATA-vs-wallet bug: `create_ata`
	/// must be built from the **wallet** pubkey, so its `accounts[1]`
	/// (the ATA slot) equals `derive_ata(wallet, mint)` and `accounts[2]`
	/// (the owner slot) equals the wallet itself.
	///
	/// Previously the handler passed `tokens.recipient_token_account` (an
	/// ATA) as `owner`, producing `derive_ata(ATA, mint)` which is a
	/// different, unused account.
	#[test]
	fn handle_buffered_create_ata_uses_wallet_not_ata() {
		let mint = Pubkey::new_unique();
		let wallet = Pubkey::new_unique();
		let fee_payer = Pubkey::new_unique();

		// The correct construction — what the handler now does.
		let correct = build_create_ata_idempotent_ix(&fee_payer, &wallet, &mint);
		// The buggy construction — what the handler used to do. We
		// reproduce it here so the test diff shows the two diverge.
		let buggy_owner = derive_ata(&wallet, &mint); // = tokens.recipient_token_account
		let buggy = build_create_ata_idempotent_ix(&fee_payer, &buggy_owner, &mint);

		// accounts[1] is the ATA address being created.
		assert_eq!(correct.accounts[1].pubkey, derive_ata(&wallet, &mint));
		assert_ne!(
			buggy.accounts[1].pubkey,
			derive_ata(&wallet, &mint),
			"buggy path derives an unrelated ATA keyed on (ATA, mint)"
		);
		// accounts[2] is the owner — must be the wallet, not an ATA.
		assert_eq!(correct.accounts[2].pubkey, wallet);
	}
}
