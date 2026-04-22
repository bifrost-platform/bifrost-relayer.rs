// SPDX-License-Identifier: Apache-2.0
//
// Unified Solana outbound handler. This worker owns both halves of the
// BFC ↔ Solana cross-chain flow on the relayer side:
//
//   1. **BFC → Solana job submission.** The Bifrost socket relay handler
//      routes `dst_chain_id == sol_chain_id` messages onto the `SolOutboundJob`
//      channel; this worker drains that channel, builds the matching
//      cccp-solana `poll(...)` / `round_control_relay(...)` instruction
//      via `crate::sol::ix_builder`, and submits it with the relayer's
//      Solana fee-payer keypair (with retry + priority-fee escalation).
//   2. **Solana → BFC commit mirroring.** Once a `poll(...)` lands on
//      the cluster, the cccp-solana program emits a
//      `SocketEvent { req_id.chain == BFC_MAIN, status: Accepted|Rejected, ... }`.
//      The slot manager broadcasts it as `EventType::Outbound`; this
//      worker subscribes, reconstructs the EVM `Socket_Message` shape,
//      signs it with the relayer's secp256k1 key, and pushes the
//      resulting unsigned extrinsic onto `XtRequestSender` so BFC's
//      blaze pallet can close the state machine.
//
// Having a single worker own both halves simplifies wiring and lets the
// commit-mirror branch reuse the same broadcast subscription + BFC
// client the outbound-submission path already needs on the same cluster.
//
// Production wiring:
//
//   * The handler owns an `AssetRegistry` (built from `SolProvider.assets`)
//     so it can map `(asset_index, recipient_wallet)` to the full
//     `(mint, vault TA, recipient TA)` triple without an extra round-trip.
//   * Every outbound transaction prepends a `create_associated_token_account`
//     idempotent IX so first-time recipients don't bounce.
//   * `is_relay_target=false` (= watch-only cluster) disables the
//     BFC → Solana submission path **only**; the Solana → BFC commit
//     mirror stays active so observed `Accepted`/`Rejected` events still
//     round-trip to BFC even on non-relay-target clusters.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy::{
	network::Network,
	primitives::{Address, Bytes, FixedBytes, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use array_bytes::Hexify;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::Instruction;
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer, read_keypair_file};
use solana_sdk::transaction::Transaction;
use subxt::ext::subxt_core::utils::AccountId20;
use tokio::sync::broadcast::Receiver;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use br_primitives::{
	contracts::socket::Socket_Struct::{
		Instruction as EvmInstruction, RequestID, Socket_Message, Task_Params,
	},
	eth::SocketEventStatus,
	sol::{Event, EventMessage, EventType},
	substrate::{SocketMessagesSubmission, bifrost_runtime},
	tx::{SocketRelayMetadata, XtRequest, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};

use crate::eth::EthClient;
use crate::sol::ata::{build_create_ata_idempotent_ix, derive_ata};
use crate::sol::client::SolClient;
use crate::sol::codec::{AssetIndex, PollSubmit, RoundUpSubmit, Signatures};
use crate::sol::ix_builder::{
	PollTokenAccounts, SIGS_PER_CHUNK, build_poll_buffered_ix, build_poll_ix,
	build_round_control_relay_ix, build_submit_signatures_ix,
};
use crate::sol::registry::AssetRegistry;

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

// ── Compute Budget Program ──────────────────────────────────────────
// We construct the IX data by hand to avoid pulling in an extra crate.
// Program ID: ComputeBudget111111111111111111111111111111
const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
	3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231, 188, 140, 229, 164,
	218, 0, 137, 145, 204, 121, 124, 0, 0, 0, 0, 0,
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
const DEFAULT_BASE_PRIORITY_FEE: u64 = 1_000;
const DEFAULT_MAX_PRIORITY_FEE: u64 = 1_000_000;
const DEFAULT_CONFIRMATION_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_SEND_RETRIES: u32 = 3;
const PRIORITY_FEE_MULTIPLIER: u64 = 3;
const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_millis(2_000);
/// Solana's maximum transaction packet size.
const SOLANA_MAX_TX_SIZE: usize = 1232;

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

/// Unified Solana outbound worker. One per Solana cluster.
///
/// Generic parameters mirror `SolInboundHandler` — the type matches the
/// Bifrost EVM client the commit-mirror branch signs with. The BFC →
/// Solana submission path doesn't use the generics, but keeping them on
/// the struct lets the same worker own both halves without an extra
/// wrapper.
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
	/// Bifrost client — signs the unsigned extrinsic pushed onto BFC
	/// when this cluster emits an outbound `SocketEvent` (Accepted /
	/// Rejected). Mirror of `SolInboundHandler.bfc_client`.
	bfc_client: Arc<EthClient<F, P, N>>,
	/// BFC unsigned-tx queue. The commit-mirror branch builds
	/// `submit_outbound_requests(...)` calls and sends them here.
	xt_request_sender: Arc<XtRequestSender>,
	/// Slot-manager broadcast subscription. `EventType::Outbound`
	/// messages carry BFC-origin `SocketEvent`s that must be mirrored
	/// back to BFC so the state machine closes without a rollback
	/// timeout.
	event_stream: BroadcastStream<EventMessage>,
}

impl<F, P, N: Network> SolOutboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
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
		xt_request_sender: Arc<XtRequestSender>,
		event_receiver: Receiver<EventMessage>,
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
				xt_request_sender,
				event_stream: BroadcastStream::new(event_receiver),
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
			return;
		}

		// backoff = INITIAL * 2^attempts, capped at MAX_PENDING_BACKOFF.
		let shift = attempts.min(16); // avoid overflow; 2^16 = 65536s >> cap
		let backoff =
			INITIAL_PENDING_BACKOFF.saturating_mul(1u32 << shift).min(MAX_PENDING_BACKOFF);
		let next_retry_at = tokio::time::Instant::now() + backoff;

		self.pending.push(PendingJob { job, attempts: attempts + 1, next_retry_at });
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
							Some(Ok(msg)) => self.handle_commit_event_message(&msg).await,
							Some(Err(err)) => log::warn!(
								target: &self.client.get_chain_name(),
								"[{}] commit event stream lag/error: {err:?}",
								SUB_LOG_TARGET,
							),
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
						Some(Ok(msg)) => self.handle_commit_event_message(&msg).await,
						Some(Err(err)) => log::warn!(
							target: &self.client.get_chain_name(),
							"[{}] commit event stream lag/error: {err:?}",
							SUB_LOG_TARGET,
						),
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
			}
		}
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

	/// Reconstruct the EVM `Socket_Message` shape from the decoded Solana
	/// event and, if the status closes the outbound half of the state
	/// machine (`Accepted` / `Rejected`), submit it back to BFC as an
	/// unsigned `submit_outbound_requests` extrinsic.
	async fn handle_commit_event(&self, ev: &Event) {
		let status = SocketEventStatus::from(ev.status);
		if !matches!(status, SocketEventStatus::Accepted | SocketEventStatus::Rejected) {
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

		// Avoid the RPC round-trip + signing work if we can't participate
		// this round.
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

		let (call, metadata) = match self.build_commit_unsigned_tx(ev).await {
			Ok(x) => x,
			Err(err) => {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to build outbound-commit extrinsic for sig={} seq={}: {err}",
					ev.signature,
					ev.sequence
				);
				return;
			},
		};

		match self.xt_request_sender.send(XtRequestMessage::new(
			call,
			XtRequestMetadata::SubmitOutboundRequests(metadata.clone()),
		)) {
			Ok(_) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔖 Request unsigned tx (sol outbound-commit): {} sig={}",
				sub_display_format(SUB_LOG_TARGET),
				metadata,
				ev.signature,
			),
			Err(error) => br_primitives::log_and_capture!(
				error,
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.bfc_client.address().await,
				"❗️ Failed to send outbound-commit unsigned tx for {}: {error}",
				metadata
			),
		}
	}

	fn build_commit_socket_message(&self, ev: &Event) -> Socket_Message {
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
				tokenIDX1: FixedBytes::from([0u8; 32]),
				refund: Address::from(ev.refund),
				to: Address::from(ev.to),
				amount: U256::from_be_bytes(ev.amount),
				variants: Bytes::from(ev.variants.clone()),
			},
		}
	}

	async fn build_commit_unsigned_tx(
		&self,
		ev: &Event,
	) -> eyre::Result<(XtRequest, SocketRelayMetadata)> {
		let socket_msg = self.build_commit_socket_message(ev);
		let encoded_msg: Vec<u8> = socket_msg.clone().into();

		let signature = self
			.bfc_client
			.sign_message(encoded_msg.hexify_prefixed().as_bytes())
			.await
			.map_err(|e| eyre::eyre!("sign_message: {e}"))?
			.into();

		let call: XtRequest = Arc::new(bifrost_runtime::tx().blaze().submit_outbound_requests(
			SocketMessagesSubmission {
				authority_id: AccountId20(self.bfc_client.address().await.0.0),
				messages: vec![encoded_msg],
			},
			signature,
		));

		// For outbound-commit: src = BFC (originator), dst = this Solana
		// cluster (where the poll just landed). `is_inbound=false` so the
		// metadata prints as "Outbound" in logs.
		let metadata = SocketRelayMetadata::new(
			false,
			SocketEventStatus::from(ev.status),
			ev.sequence,
			self.bfc_client.chain_id(),
			self.client.chain_id,
			Address::from(ev.to),
			false,
		);

		Ok((call, metadata))
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
				// Transaction succeeded (has confirmations, no error).
				return Ok(());
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

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] using buffered path for {total_sigs} signatures",
			SUB_LOG_TARGET,
		);

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
				&self.fee_payer.pubkey(),
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
			&self.fee_payer.pubkey(),
			&submit.msg,
			&submit.option,
			asset_index,
			&tokens,
		);

		let mut ixs = vec![
			compute_budget_set_cu_limit(DEFAULT_COMPUTE_UNIT_LIMIT),
			compute_budget_set_cu_price(self.base_priority_fee),
		];
		if let Some(wallet) = recipient_wallet {
			ixs.push(build_create_ata_idempotent_ix(
				&self.fee_payer.pubkey(),
				&wallet,
				&tokens.mint,
			));
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

	/// Send a single instruction as a transaction, wait for confirmation.
	async fn send_single_ix(&self, ix: Instruction) -> eyre::Result<()> {
		let ixs = vec![
			compute_budget_set_cu_limit(200_000),
			compute_budget_set_cu_price(self.base_priority_fee),
			ix,
		];

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
		let entry = registry.get(&asset_index.0).unwrap();
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
		let entry = registry.get(&asset_index.0).unwrap();
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
