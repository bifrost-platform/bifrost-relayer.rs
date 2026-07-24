// SPDX-License-Identifier: Apache-2.0
//
// Slot polling loop. Mirror of `client/src/btc/block.rs::BlockManager`.
//
// On every tick (`call_interval` ms) the worker:
//   1. Fetches the latest slot from the configured Solana cluster.
//   2. Walks newly finalized signatures touching the cccp-solana
//      program ID via `getSignaturesForAddress` (with `until` set to the
//      most recently processed signature so we never re-emit duplicates).
//   3. For each signature, fetches the full transaction (`getTransaction`)
//      and runs `crate::sol::decoder::decode_anchor_events` over its
//      `log_messages`.
//   4. Wraps the decoded events into `br_primitives::sol::EventMessage`
//      and broadcasts them on the slot channel for the handler workers
//      to consume.
//
// Solana RPC commitment is fixed to `finalized`, so every transaction
// returned by `getSignaturesForAddress` is already past finality.

use std::sync::{
	Arc, Mutex,
	atomic::{AtomicUsize, Ordering},
};

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_client::rpc_config::RpcTransactionConfig;
use solana_commitment_config::CommitmentConfig;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_sdk::signature::Signature;
use solana_transaction_status_client_types::{
	EncodedConfirmedTransactionWithStatusMeta, UiTransactionEncoding,
};
use std::str::FromStr;
use tokio::sync::{
	mpsc::{self, UnboundedReceiver, UnboundedSender},
	oneshot,
};

use br_primitives::sol::{Event, EventMessage, EventType};

use crate::sol::client::SolClient;
use crate::sol::decoder::{DecodedAnchorEvent, decode_successful_anchor_events};

const SUB_LOG_TARGET: &str = "slot-manager";
/// How long to run HTTP polling before retrying a dropped WS connection.
const WS_RECONNECT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
/// Maximum gap allowed between WS `slotSubscribe` notifications before
/// the watchdog declares the stream stalled and forces an HTTP fallback.
/// Solana mainnet produces a slot every ~400ms; slot notifications
/// typically arrive within a few seconds, so a 10-second window
/// is long enough to avoid false positives on a slow cluster but tight
/// enough to recover from a silently-dead WS pipe within one cycle.
const WS_NOTIFICATION_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(10);

fn reached_stateless_floor(
	has_signature_cursor: bool,
	oldest_page_slot: Option<u64>,
	floor_slot: u64,
) -> bool {
	!has_signature_cursor && oldest_page_slot.map(|slot| slot <= floor_slot).unwrap_or(true)
}

/// One delivery from `SlotManager` to a consumer. Dropping a delivery
/// without calling `acknowledge` keeps the in-memory batch pending for the
/// next tick. A process restart replays the configured finalized bootstrap
/// window and relies on on-chain idempotency instead of local persistence.
pub struct EventDelivery {
	pub message: EventMessage,
	ack: Option<oneshot::Sender<()>>,
}

impl EventDelivery {
	pub fn acknowledge(mut self) {
		if let Some(ack) = self.ack.take() {
			let _ = ack.send(());
		}
	}
}

/// Slot poller — produces `EventMessage` items from the configured
/// Solana cluster.
pub struct SlotManager {
	pub client: SolClient,
	rpc: Arc<RpcClient>,
	/// Per-consumer unbounded queues. A slow consumer retains its own
	/// backlog instead of causing `broadcast::Lagged` and silently losing
	/// an irrevocable bridge event.
	subscribers: Mutex<Vec<UnboundedSender<EventDelivery>>>,
	/// Number of consumers registered for this manager. In-memory cursor
	/// advancement requires an ACK from every one; a closed queue therefore
	/// stalls ingestion instead of silently dropping one side of the bridge.
	required_subscribers: AtomicUsize,
	/// Slot polling interval in milliseconds.
	call_interval: u64,
	/// Most recent signature we've already streamed events for. Passed
	/// as `until` on the next `getSignaturesForAddress` call so we only
	/// fetch the delta.
	last_seen_signature: Option<Signature>,
	/// Most recent slot we've already streamed events for. Used as a
	/// monotonic checkpoint for `NewSlot` heartbeats.
	last_processed_slot: u64,
	/// Current decoded batch, retained in memory until every consumer ACKs.
	pending_messages: Vec<EventMessage>,
	/// Optional WebSocket URL for `slotSubscribe`. When set, the manager
	/// uses real-time WS notifications to trigger ticks instead of polling
	/// `getSlot` over HTTP. Falls back to HTTP on WS failure.
	ws_url: Option<String>,
}

impl SlotManager {
	pub fn new(client: SolClient, call_interval: u64, ws_url: Option<String>) -> Self {
		let rpc = client.rpc();
		Self {
			client,
			rpc,
			subscribers: Mutex::new(Vec::new()),
			required_subscribers: AtomicUsize::new(0),
			call_interval,
			last_seen_signature: None,
			last_processed_slot: 0,
			pending_messages: Vec::new(),
			ws_url,
		}
	}

	/// Subscribe to the slot event channel. Multiple consumers (inbound
	/// handler, outbound roundup handler, metrics worker) can subscribe
	/// in parallel.
	pub fn subscribe(&self) -> UnboundedReceiver<EventDelivery> {
		let (sender, receiver) = mpsc::unbounded_channel();
		self.subscribers.lock().expect("subscriber mutex poisoned").push(sender);
		self.required_subscribers.fetch_add(1, Ordering::Relaxed);
		receiver
	}

	async fn publish(&self, message: EventMessage) -> eyre::Result<()> {
		let required = self.required_subscribers.load(Ordering::Relaxed);
		if required == 0 {
			eyre::bail!("no Solana event consumers are registered");
		}

		let mut acknowledgements = Vec::with_capacity(required);
		let mut send_failed = false;
		{
			let mut subscribers = self.subscribers.lock().expect("subscriber mutex poisoned");
			subscribers.retain(|sender| {
				let (ack, received) = oneshot::channel();
				match sender.send(EventDelivery { message: message.clone(), ack: Some(ack) }) {
					Ok(()) => {
						acknowledgements.push(received);
						true
					},
					Err(_) => {
						send_failed = true;
						false
					},
				}
			});
		}

		if send_failed || acknowledgements.len() != required {
			eyre::bail!(
				"Solana event delivery reached {}/{} required consumers",
				acknowledgements.len(),
				required
			);
		}
		for acknowledgement in acknowledgements {
			acknowledgement
				.await
				.map_err(|_| eyre::eyre!("Solana event consumer dropped delivery without ACK"))?;
		}
		Ok(())
	}

	/// Deliver the current in-memory batch and clear it only after every
	/// registered consumer ACKs every message. On process restart the
	/// bootstrap window reconstructs the batch from finalized chain history.
	async fn replay_pending(&mut self) -> eyre::Result<()> {
		if self.pending_messages.is_empty() {
			return Ok(());
		}

		for message in self.pending_messages.clone() {
			self.publish(message).await?;
		}
		self.pending_messages.clear();
		Ok(())
	}

	/// Run the slot manager. If a `ws_url` is configured, the manager
	/// first tries a WebSocket `slotSubscribe` connection for real-time
	/// slot notifications. On WS failure it automatically falls back to
	/// the HTTP polling loop and periodically retries the WS connection.
	pub async fn run(&mut self) -> eyre::Result<()> {
		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] slot manager started; program={} ws={}",
			SUB_LOG_TARGET,
			self.client.program_id,
			self.ws_url.as_deref().unwrap_or("none (HTTP polling)"),
		);

		loop {
			if let Some(ws_url) = &self.ws_url {
				match self.run_ws_loop(ws_url.clone()).await {
					Ok(()) => {
						// run_ws_loop only returns Ok if the stream ends gracefully.
						log::warn!(
							target: &self.client.get_chain_name(),
							"[{}] WS slotSubscribe stream ended; falling back to HTTP polling",
							SUB_LOG_TARGET,
						);
					},
					Err(err) => {
						log::warn!(
							target: &self.client.get_chain_name(),
							"[{}] WS slotSubscribe failed: {err}; falling back to HTTP polling",
							SUB_LOG_TARGET,
						);
					},
				}
			}

			// HTTP polling fallback (or primary if no ws_url).
			// Run for `WS_RECONNECT_INTERVAL` then try WS again if configured.
			if self.ws_url.is_some() {
				self.run_http_polling_for(WS_RECONNECT_INTERVAL).await;
			} else {
				self.run_http_polling().await;
				// run_http_polling never returns unless something catastrophic
				// happens; break out to let the supervisor handle it.
				break;
			}
		}

		Ok(())
	}

	/// HTTP polling loop — runs indefinitely.
	async fn run_http_polling(&mut self) {
		let mut ticker =
			tokio::time::interval(std::time::Duration::from_millis(self.call_interval.max(100)));
		loop {
			ticker.tick().await;
			if let Err(err) = self.tick().await {
				log::warn!(
					target: &self.client.get_chain_name(),
					"[{}] tick failed: {err:?}",
					SUB_LOG_TARGET,
				);
			}
		}
	}

	/// HTTP polling loop — runs for the given duration then returns so
	/// the caller can retry a WS connection.
	async fn run_http_polling_for(&mut self, duration: std::time::Duration) {
		let deadline = tokio::time::Instant::now() + duration;
		let mut ticker =
			tokio::time::interval(std::time::Duration::from_millis(self.call_interval.max(100)));
		loop {
			ticker.tick().await;
			if tokio::time::Instant::now() >= deadline {
				return;
			}
			if let Err(err) = self.tick().await {
				log::warn!(
					target: &self.client.get_chain_name(),
					"[{}] tick failed: {err:?}",
					SUB_LOG_TARGET,
				);
			}
		}
	}

	/// WebSocket-driven loop. Connects to the cluster's WS endpoint,
	/// subscribes to `slotSubscribe`, and fires a `tick()` on every
	/// notification. Returns when the stream ends, an error occurs, or
	/// the notification watchdog fires.
	///
	/// The watchdog protects against silently-dead WS connections: the
	/// underlying tokio-tungstenite socket can stay up while the Solana
	/// node stops pushing updates (observed on some RPC providers under
	/// load). If no notification arrives within
	/// `WS_NOTIFICATION_WATCHDOG`, we return `Err` so the caller falls
	/// back to HTTP polling and re-establishes the WS connection on the
	/// next reconnect cycle.
	async fn run_ws_loop(&mut self, ws_url: String) -> eyre::Result<()> {
		use tokio_stream::StreamExt;

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] connecting to WS: {ws_url}",
			SUB_LOG_TARGET,
		);
		let pubsub =
			PubsubClient::new(&ws_url).await.map_err(|e| eyre::eyre!("WS connect: {e}"))?;

		let (mut stream, _unsub) =
			pubsub.slot_subscribe().await.map_err(|e| eyre::eyre!("slotSubscribe: {e}"))?;

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] WS slotSubscribe active (watchdog={WS_NOTIFICATION_WATCHDOG:?})",
			SUB_LOG_TARGET,
		);

		loop {
			match tokio::time::timeout(WS_NOTIFICATION_WATCHDOG, stream.next()).await {
				Ok(Some(slot_info)) => {
					// The WS notification gives us the slot number directly.
					// We still need to fetch signatures + transactions via
					// HTTP RPC, but we save the `getSlot` call and gain
					// sub-second latency compared to HTTP polling.
					log::trace!(
						target: &self.client.get_chain_name(),
						"[{}] WS slot notification: slot={}",
						SUB_LOG_TARGET,
						slot_info.slot,
					);

					if slot_info.slot <= self.last_processed_slot {
						continue;
					}

					if let Err(err) = self.tick_at_slot(slot_info.slot).await {
						log::warn!(
							target: &self.client.get_chain_name(),
							"[{}] WS tick failed at slot {}: {err:?}",
							SUB_LOG_TARGET,
							slot_info.slot,
						);
					}
				},
				Ok(None) => {
					// Stream ended gracefully — let the outer `run()` loop
					// fall back to HTTP polling, then retry WS.
					return Ok(());
				},
				Err(_elapsed) => {
					// Watchdog fired. Treat this as a transport failure so
					// the caller falls back to HTTP polling and we
					// re-establish the WS handshake on the next cycle.
					return Err(eyre::eyre!(
						"WS slotSubscribe stalled — no notification within {WS_NOTIFICATION_WATCHDOG:?}"
					));
				},
			}
		}
	}

	/// One polling tick: refresh slot via HTTP, then process.
	pub async fn tick(&mut self) -> eyre::Result<()> {
		let latest_slot = self.rpc.get_slot().await.map_err(|e| eyre::eyre!("get_slot: {e}"))?;

		if latest_slot <= self.last_processed_slot {
			return Ok(());
		}

		self.tick_at_slot(latest_slot).await
	}

	/// Process a specific slot: fetch new signatures, decode each
	/// transaction's logs, broadcast the resulting events.
	pub async fn tick_at_slot(&mut self, latest_slot: u64) -> eyre::Result<()> {
		// Redeliver an in-memory batch whose consumer ACK failed before
		// fetching anything newer.
		self.replay_pending().await?;

		// Fetch every signature touching the program since `last_seen`.
		// `getSignaturesForAddress` is page-limited, so walk `before` until
		// the prior in-memory cursor is reached. If no signature cursor exists
		// (for example, bootstrap found no traffic), paginate until the
		// previously processed slot boundary instead of fetching one page and
		// potentially skipping a busy interval.
		let batch = (self.client.get_signatures_batch_size as usize).clamp(1, 1_000);
		let until = self.last_seen_signature;
		let floor_slot = self.last_processed_slot;
		let mut before = None;
		let mut sigs = Vec::new();
		loop {
			let cfg = GetConfirmedSignaturesForAddress2Config {
				before,
				until,
				limit: Some(batch),
				commitment: Some(CommitmentConfig::finalized()),
			};
			let page = self
				.rpc
				.get_signatures_for_address_with_config(&self.client.program_id, cfg)
				.await
				.map_err(|e| eyre::eyre!("get_signatures_for_address: {e}"))?;
			let page_len = page.len();
			if page_len == 0 {
				break;
			}
			let reached_floor = reached_stateless_floor(
				until.is_some(),
				page.last().map(|entry| entry.slot),
				floor_slot,
			);
			before = Some(
				Signature::from_str(&page.last().expect("non-empty page").signature)
					.map_err(|e| eyre::eyre!("RPC returned invalid signature: {e}"))?,
			);
			sigs.extend(page);
			if reached_floor || page_len < batch {
				break;
			}
		}
		if until.is_none() {
			sigs.retain(|entry| entry.slot > floor_slot);
		}

		// Publish slot as a `block_height` gauge so the existing EVM
		// Grafana panels pick up the Solana cluster alongside the EVM
		// chains. The label is the cluster name, matching the shape
		// every other `relayer_block_height` entry uses.
		br_metrics::set_block_height(&self.client.name, latest_slot);

		if sigs.is_empty() {
			// Nothing happened — emit a NewSlot heartbeat so handlers can
			// tell apart "no events yet" from "RPC stalled".
			self.publish(EventMessage::new_slot(latest_slot)).await?;
			br_metrics::increase_sol_events(&self.client.name, "new_slot");
			self.last_processed_slot = latest_slot;
			return Ok(());
		}

		// The RPC returns sigs in newest-first order. Process oldest first
		// so subscribers see events in chronological order.
		let mut new_signatures: Vec<_> = sigs.into_iter().collect();
		new_signatures.reverse();

		let mut pending_messages = Vec::new();
		for entry in &new_signatures {
			let sig_str = entry.signature.clone();
			let slot = entry.slot;
			if let Some(err) = &entry.err {
				log::debug!(
					target: &self.client.get_chain_name(),
					"[{}] ignoring failed transaction {sig_str}: {err:?}",
					SUB_LOG_TARGET,
				);
				continue;
			}
			let signature = Signature::from_str(&sig_str)
				.map_err(|err| eyre::eyre!("unparseable signature {sig_str}: {err}"))?;

			let tx = self
				.rpc
				.get_transaction_with_config(
					&signature,
					RpcTransactionConfig {
						encoding: Some(UiTransactionEncoding::Json),
						commitment: Some(CommitmentConfig::finalized()),
						max_supported_transaction_version: Some(0),
					},
				)
				.await
				.map_err(|err| eyre::eyre!("get_transaction({sig_str}): {err}"))?;

			pending_messages.extend(self.decode_transaction(slot, &sig_str, &tx)?);
		}

		// Update the cursor to the newest sig (which is the last in our
		// chronologically-sorted list).
		let latest = Signature::from_str(&new_signatures.last().expect("non-empty list").signature)
			.map_err(|e| eyre::eyre!("invalid newest signature from RPC: {e}"))?;
		// Advance only the in-memory cursor. A process restart reconstructs
		// this interval through `bootstrap_catchup`; duplicate deliveries are
		// filtered using BFC/Solana on-chain state.
		self.last_seen_signature = Some(latest);
		self.last_processed_slot = latest_slot;
		self.pending_messages = pending_messages;
		self.replay_pending().await
	}

	/// Walk backwards from the current slot until we cross the requested
	/// `offset_slots` boundary, processing every cccp-solana transaction
	/// in between. The caller derives `offset_slots` from the global BFC
	/// bootstrap round horizon and recent Solana slot timing.
	///
	/// The RPC returns signatures newest-first and we paginate via
	/// `before`, so the walk is:
	///
	///   cursor = None                           # = "start from tip"
	///   loop:
	///     page = getSignaturesForAddress(before=cursor, limit=batch)
	///     if page empty: break
	///     if oldest entry in page is below deadline: stop after this page
	///     cursor = page.last().signature
	///
	/// We then process the full catch-up range in chronological order so
	/// subscribers see events in the same order `tick_at_slot` would
	/// deliver them live, and update `last_seen_signature` to the
	/// newest signature we saw (so the next live tick starts from that
	/// in-memory cursor, not duplicate what we already replayed).
	pub async fn bootstrap_catchup(&mut self, offset_slots: u64) -> eyre::Result<()> {
		self.replay_pending().await?;
		if self.last_seen_signature.is_some() {
			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] bootstrap already completed in memory at slot {}; skipping duplicate replay",
				SUB_LOG_TARGET,
				self.last_processed_slot,
			);
			return Ok(());
		}
		if offset_slots == 0 {
			return Ok(());
		}

		let tip = self.rpc.get_slot().await.map_err(|e| eyre::eyre!("get_slot: {e}"))?;

		// Saturating arithmetic — a fresh cluster may have slot numbers
		// smaller than the offset.
		let deadline_slot = tip.saturating_sub(offset_slots);

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] bootstrap catch-up: tip={tip} deadline={deadline_slot} (offset={offset_slots})",
			SUB_LOG_TARGET,
		);

		// Collect all pages into a single chronologically-sorted list
		// before processing so subscribers receive events in the same
		// order live polling would produce.
		let mut collected: Vec<
			solana_client::rpc_response::RpcConfirmedTransactionStatusWithSignature,
		> = Vec::new();
		let batch = (self.client.get_signatures_batch_size as usize).clamp(1, 1_000);
		let mut before: Option<Signature> = None;

		loop {
			let cfg = GetConfirmedSignaturesForAddress2Config {
				before,
				until: None,
				limit: Some(batch),
				commitment: Some(CommitmentConfig::finalized()),
			};
			let page = self
				.rpc
				.get_signatures_for_address_with_config(&self.client.program_id, cfg)
				.await
				.map_err(|e| eyre::eyre!("bootstrap get_signatures_for_address: {e}"))?;

			if page.is_empty() {
				break;
			}

			let page_oldest_slot = page.last().map(|e| e.slot).unwrap_or(0);
			let page_newest_sig = page.first().map(|e| e.signature.clone());

			collected.extend(page);

			if page_oldest_slot <= deadline_slot {
				// We've walked back past the deadline; stop paginating.
				break;
			}

			// Move cursor to the oldest sig in this page so the next
			// getSignaturesForAddress fetches strictly older entries.
			before = page_newest_sig.as_ref().and_then(|_| {
				collected.last().and_then(|e| Signature::from_str(&e.signature).ok())
			});
			if before.is_none() {
				break;
			}
		}

		// Drop entries older than the deadline — they're from beyond the
		// configured offset and we don't replay them.
		collected.retain(|e| e.slot > deadline_slot);

		if collected.is_empty() {
			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] bootstrap catch-up: no cccp-solana traffic in the last {offset_slots} slots",
				SUB_LOG_TARGET,
			);
			self.last_processed_slot = tip;
			return Ok(());
		}

		// RPC returned newest-first; reverse so we replay oldest-first.
		collected.reverse();

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] bootstrap catch-up: replaying {} signatures from slot {}..={}",
			SUB_LOG_TARGET,
			collected.len(),
			collected.first().map(|e| e.slot).unwrap_or(0),
			collected.last().map(|e| e.slot).unwrap_or(0),
		);

		let mut pending_messages = Vec::new();
		for entry in &collected {
			let sig_str = entry.signature.clone();
			let slot = entry.slot;
			if let Some(err) = &entry.err {
				log::debug!(
					target: &self.client.get_chain_name(),
					"[{}] bootstrap: ignoring failed transaction {sig_str}: {err:?}",
					SUB_LOG_TARGET,
				);
				continue;
			}
			let signature = Signature::from_str(&sig_str)
				.map_err(|err| eyre::eyre!("bootstrap unparseable signature {sig_str}: {err}"))?;

			let tx = self
				.rpc
				.get_transaction_with_config(
					&signature,
					RpcTransactionConfig {
						encoding: Some(UiTransactionEncoding::Json),
						commitment: Some(CommitmentConfig::finalized()),
						max_supported_transaction_version: Some(0),
					},
				)
				.await
				.map_err(|err| eyre::eyre!("bootstrap get_transaction({sig_str}): {err}"))?;

			pending_messages.extend(self.decode_transaction(slot, &sig_str, &tx)?);
		}

		// Advance cursors so the live tick picks up from where catch-up
		// left off — i.e. the newest signature we saw, and the tip slot.
		let newest = Signature::from_str(&collected.last().expect("non-empty list").signature)
			.map_err(|e| eyre::eyre!("invalid newest bootstrap signature: {e}"))?;
		self.last_seen_signature = Some(newest);
		self.last_processed_slot = tip;
		self.pending_messages = pending_messages;
		self.replay_pending().await
	}

	fn decode_transaction(
		&self,
		slot: u64,
		signature: &str,
		tx: &EncodedConfirmedTransactionWithStatusMeta,
	) -> eyre::Result<Vec<EventMessage>> {
		let meta = tx
			.transaction
			.meta
			.as_ref()
			.ok_or_else(|| eyre::eyre!("transaction {signature} has no status metadata"))?;
		let transaction_failed = meta.err.is_some();
		let logs: Vec<String> =
			Option::<Vec<String>>::from(meta.log_messages.clone()).unwrap_or_default();

		let decoded =
			decode_successful_anchor_events(&self.client.program_id, transaction_failed, &logs);
		if decoded.is_empty() {
			return Ok(Vec::new());
		}

		let mut inbound_events: Vec<Event> = Vec::new();
		let mut outbound_events: Vec<Event> = Vec::new();
		let mut asset_directory_updated = false;

		for ev in &decoded {
			let msg = match ev {
				DecodedAnchorEvent::Socket(msg) => msg,
				DecodedAnchorEvent::AssetDirectoryUpdated(_) => {
					asset_directory_updated = true;
					continue;
				},
				// RoundUp events are routed by a different worker.
				DecodedAnchorEvent::RoundUp(_) => continue,
			};

			let evt = Event {
				signature: signature.to_string(),
				slot,
				req_chain: msg.req_id.chain.0,
				round_id: msg.req_id.round_id,
				sequence: msg.req_id.sequence,
				status: msg.status,
				ins_code_chain: msg.ins_code.chain.0,
				ins_code_method: msg.ins_code.method.0,
				asset_index: msg.params.token_idx0.0,
				token_idx1: msg.params.token_idx1.0,
				to: msg.params.to,
				refund: msg.params.refund,
				amount: msg.params.amount,
				variants: msg.params.variants.clone(),
			};

			// Inbound = the request originated on this chain (req_id.chain
			// is our this_chain). Outbound = the request was created on
			// another chain (req_id.chain is one of the BFC tracks or
			// another remote).
			//
			// We don't have direct access to `this_chain` here without
			// an RPC roundtrip; instead we use the convention that any
			// recognized BFC ChainIndex (mainnet `0x00000bfc` or
			// testnet `0x0000bfc0`) means "outbound" and anything else
			// means "inbound" (the user-side `request` IX always sets
			// req_id.chain = the cluster's SOL_DEV/SOL_TEST/SOL_MAIN value).
			// The matching test on the
			// on-chain side is `code_chain == SocketConfig.bfc_chain_index`
			// — that locks the pairing to one BFC at deploy time, but
			// the relayer accepts either so a single binary works on
			// both tracks without a config flag.
			let is_bfc_origin =
				matches!(msg.req_id.chain.0, [0x00, 0x00, 0x0b, 0xfc] | [0x00, 0x00, 0xbf, 0xc0]);
			if is_bfc_origin {
				outbound_events.push(evt);
			} else {
				inbound_events.push(evt);
			}
		}

		let mut messages = Vec::with_capacity(3);
		if asset_directory_updated {
			messages.push(EventMessage::asset_directory_updated(slot));
			br_metrics::increase_sol_events(&self.client.name, "asset_directory_updated");
		}
		if !inbound_events.is_empty() {
			let count = inbound_events.len() as u64;
			messages.push(EventMessage::new(slot, EventType::Inbound, inbound_events));
			for _ in 0..count {
				br_metrics::increase_sol_events(&self.client.name, "inbound");
			}
		}
		if !outbound_events.is_empty() {
			let count = outbound_events.len() as u64;
			messages.push(EventMessage::new(slot, EventType::Outbound, outbound_events));
			for _ in 0..count {
				br_metrics::increase_sol_events(&self.client.name, "outbound");
			}
		}
		Ok(messages)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn stateless_floor_paginates_until_processed_slot() {
		assert!(!reached_stateless_floor(false, Some(101), 100));
		assert!(reached_stateless_floor(false, Some(100), 100));
		assert!(reached_stateless_floor(false, Some(99), 100));
		assert!(!reached_stateless_floor(true, Some(99), 100));
	}

	#[tokio::test]
	async fn delivery_requires_explicit_consumer_ack() {
		let (ack, received) = oneshot::channel();
		let delivery = EventDelivery { message: EventMessage::new_slot(1), ack: Some(ack) };
		delivery.acknowledge();
		assert!(received.await.is_ok());

		let (ack, received) = oneshot::channel();
		drop(EventDelivery { message: EventMessage::new_slot(2), ack: Some(ack) });
		assert!(received.await.is_err());
	}
}
