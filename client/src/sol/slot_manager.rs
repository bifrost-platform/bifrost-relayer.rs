// SPDX-License-Identifier: Apache-2.0
//
// Slot polling loop. Mirror of `client/src/btc/block.rs::BlockManager`.
//
// On every tick (`call_interval` ms) the worker:
//   1. Fetches the latest slot from the configured Solana cluster.
//   2. Walks "newly confirmed" signatures touching the cccp-solana
//      program ID via `getSignaturesForAddress` (with `until` set to the
//      most recently processed signature so we never re-emit duplicates).
//   3. For each signature, fetches the full transaction (`getTransaction`)
//      and runs `crate::sol::decoder::decode_anchor_events` over its
//      `log_messages`.
//   4. Wraps the decoded events into `br_primitives::sol::EventMessage`
//      and broadcasts them on the slot channel for inbound/outbound
//      handlers to consume.
//
// `confirmation_depth` is informational at the moment — Solana's RPC
// already filters by `commitment` (= `finalized` by default for cccp), so
// any tx returned by `getSignaturesForAddress` is already finalized when
// the commitment level is `finalized`.

use std::sync::Arc;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_sdk::signature::Signature;
use solana_transaction_status_client_types::{
	EncodedConfirmedTransactionWithStatusMeta, UiTransactionEncoding,
};
use std::str::FromStr;
use tokio::sync::broadcast::{self, Receiver, Sender};

use br_primitives::sol::{Event, EventMessage, EventType};

use crate::sol::client::SolClient;
use crate::sol::decoder::{DecodedAnchorEvent, decode_anchor_events};

const SUB_LOG_TARGET: &str = "slot-manager";
/// How long to run HTTP polling before retrying a dropped WS connection.
const WS_RECONNECT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
/// Maximum gap allowed between WS `slotSubscribe` notifications before
/// the watchdog declares the stream stalled and forces an HTTP fallback.
/// Solana mainnet produces a slot every ~400ms; confirmed-commitment
/// updates typically arrive within a few seconds, so a 10-second window
/// is long enough to avoid false positives on a slow cluster but tight
/// enough to recover from a silently-dead WS pipe within one cycle.
const WS_NOTIFICATION_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(10);

/// Slot poller — produces `EventMessage` items from the configured
/// Solana cluster.
pub struct SlotManager {
	pub client: SolClient,
	rpc: Arc<RpcClient>,
	sender: Sender<EventMessage>,
	/// Slot polling interval in milliseconds.
	call_interval: u64,
	/// Slot confirmation depth — informational. With `commitment=finalized`
	/// every transaction returned by `getSignaturesForAddress` is already
	/// past finality, so we use this only for log/heartbeat throttling.
	#[allow(dead_code)]
	confirmation_depth: u64,
	/// Most recent signature we've already streamed events for. Passed
	/// as `until` on the next `getSignaturesForAddress` call so we only
	/// fetch the delta.
	last_seen_signature: Option<Signature>,
	/// Most recent slot we've already streamed events for. Used as a
	/// monotonic checkpoint for `NewSlot` heartbeats.
	last_processed_slot: u64,
	/// Optional WebSocket URL for `slotSubscribe`. When set, the manager
	/// uses real-time WS notifications to trigger ticks instead of polling
	/// `getSlot` over HTTP. Falls back to HTTP on WS failure.
	ws_url: Option<String>,
}

impl SlotManager {
	pub fn new(
		client: SolClient,
		call_interval: u64,
		confirmation_depth: u64,
		ws_url: Option<String>,
	) -> Self {
		let rpc = client.rpc();
		let (sender, _receiver) = broadcast::channel(512);
		Self {
			client,
			rpc,
			sender,
			call_interval,
			confirmation_depth,
			last_seen_signature: None,
			last_processed_slot: 0,
			ws_url,
		}
	}

	/// Subscribe to the slot event channel. Multiple consumers (inbound
	/// handler, outbound roundup handler, metrics worker) can subscribe
	/// in parallel.
	pub fn subscribe(&self) -> Receiver<EventMessage> {
		self.sender.subscribe()
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
		// fetch every signature touching the program since `last_seen`
		let cfg = GetConfirmedSignaturesForAddress2Config {
			before: None,
			until: self.last_seen_signature,
			limit: Some(self.client.get_signatures_batch_size as usize),
			commitment: None, // inherits from rpc client
		};
		let sigs = self
			.rpc
			.get_signatures_for_address_with_config(&self.client.program_id, cfg)
			.await
			.map_err(|e| eyre::eyre!("get_signatures_for_address: {e}"))?;

		// Publish slot as a `block_height` gauge so the existing EVM
		// Grafana panels pick up the Solana cluster alongside the EVM
		// chains. The label is the cluster name, matching the shape
		// every other `relayer_block_height` entry uses.
		br_metrics::set_block_height(&self.client.name, latest_slot);

		if sigs.is_empty() {
			// Nothing happened — emit a NewSlot heartbeat so handlers can
			// tell apart "no events yet" from "RPC stalled".
			let _ = self.sender.send(EventMessage::new_slot(latest_slot));
			br_metrics::increase_sol_events(&self.client.name, "new_slot");
			self.last_processed_slot = latest_slot;
			return Ok(());
		}

		// The RPC returns sigs in newest-first order. Process oldest first
		// so subscribers see events in chronological order.
		let mut new_signatures: Vec<_> = sigs.into_iter().collect();
		new_signatures.reverse();

		for entry in &new_signatures {
			let sig_str = entry.signature.clone();
			let slot = entry.slot;
			let signature = match Signature::from_str(&sig_str) {
				Ok(s) => s,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"[{}] skipping unparseable signature {sig_str}: {err}",
						SUB_LOG_TARGET,
					);
					continue;
				},
			};

			let tx = match self.rpc.get_transaction(&signature, UiTransactionEncoding::Json).await {
				Ok(tx) => tx,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"[{}] get_transaction({sig_str}) failed: {err}",
						SUB_LOG_TARGET,
					);
					continue;
				},
			};

			self.process_transaction(slot, &sig_str, &tx);
		}

		// Update the cursor to the newest sig (which is the last in our
		// chronologically-sorted list).
		if let Some(latest) =
			new_signatures.last().and_then(|e| Signature::from_str(&e.signature).ok())
		{
			self.last_seen_signature = Some(latest);
		}
		self.last_processed_slot = latest_slot;
		Ok(())
	}

	/// Walk backwards from the current slot until we cross the requested
	/// `offset_slots` boundary, processing every cccp-solana transaction
	/// in between. Called once at boot when
	/// `SolProvider.bootstrap_offset_slots` is set.
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
	/// cursor, not duplicate what we already replayed).
	pub async fn bootstrap_catchup(&mut self, offset_slots: u64) -> eyre::Result<()> {
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
		// before processing so inbound/outbound subscribers receive
		// events in the same order live polling would produce.
		let mut collected: Vec<
			solana_client::rpc_response::RpcConfirmedTransactionStatusWithSignature,
		> = Vec::new();
		let batch = self.client.get_signatures_batch_size as usize;
		let mut before: Option<Signature> = None;

		loop {
			let cfg = GetConfirmedSignaturesForAddress2Config {
				before,
				until: None,
				limit: Some(batch),
				commitment: None,
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

		for entry in &collected {
			let sig_str = entry.signature.clone();
			let slot = entry.slot;
			let Ok(signature) = Signature::from_str(&sig_str) else {
				log::warn!(
					target: &self.client.get_chain_name(),
					"[{}] bootstrap: unparseable signature {sig_str}",
					SUB_LOG_TARGET,
				);
				continue;
			};

			let tx = match self.rpc.get_transaction(&signature, UiTransactionEncoding::Json).await {
				Ok(tx) => tx,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"[{}] bootstrap get_transaction({sig_str}) failed: {err}",
						SUB_LOG_TARGET,
					);
					continue;
				},
			};

			self.process_transaction(slot, &sig_str, &tx);
		}

		// Advance cursors so the live tick picks up from where catch-up
		// left off — i.e. the newest signature we saw, and the tip slot.
		if let Some(newest) = collected.last().and_then(|e| Signature::from_str(&e.signature).ok())
		{
			self.last_seen_signature = Some(newest);
		}
		self.last_processed_slot = tip;
		Ok(())
	}

	fn process_transaction(
		&self,
		slot: u64,
		signature: &str,
		tx: &EncodedConfirmedTransactionWithStatusMeta,
	) {
		let logs: Vec<String> = tx
			.transaction
			.meta
			.as_ref()
			.and_then(|meta| Option::<Vec<String>>::from(meta.log_messages.clone()))
			.unwrap_or_default();

		let decoded = decode_anchor_events(&logs);
		if decoded.is_empty() {
			return;
		}

		let mut inbound_events: Vec<Event> = Vec::new();
		let mut outbound_events: Vec<Event> = Vec::new();

		for ev in &decoded {
			let DecodedAnchorEvent::Socket(msg) = ev else {
				// RoundUp events are routed by a different worker. Skip
				// them here so the inbound/outbound channel stays clean.
				continue;
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
				to: msg.params.to,
				refund: msg.params.refund,
				amount: msg.params.amount,
				variants: msg.params.variants.clone(),
			};

			// Inbound = the request originated on this chain (req_id.chain
			// is our this_chain). Outbound = the request was created on
			// another chain (req_id.chain is BFC_MAIN or another remote).
			//
			// We don't have direct access to `this_chain` here without an
			// RPC roundtrip; instead we use the convention that BFC_MAIN
			// (0x00000bfc) means "outbound" and anything else means
			// "inbound" (the user-side `request` IX always sets
			// req_id.chain = SOL_MAIN). This matches the on-chain
			// `_switch_poll_type` dispatch precisely.
			if msg.req_id.chain.0 == [0x00, 0x00, 0x0b, 0xfc] {
				outbound_events.push(evt);
			} else {
				inbound_events.push(evt);
			}
		}

		if !inbound_events.is_empty() {
			let count = inbound_events.len() as u64;
			let _ = self.sender.send(EventMessage::new(slot, EventType::Inbound, inbound_events));
			for _ in 0..count {
				br_metrics::increase_sol_events(&self.client.name, "inbound");
			}
		}
		if !outbound_events.is_empty() {
			let count = outbound_events.len() as u64;
			let _ = self.sender.send(EventMessage::new(slot, EventType::Outbound, outbound_events));
			for _ in 0..count {
				br_metrics::increase_sol_events(&self.client.name, "outbound");
			}
		}
	}
}
