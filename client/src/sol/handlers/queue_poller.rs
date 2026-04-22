// SPDX-License-Identifier: Apache-2.0
//
// Solana counterpart of `eth::handlers::SocketQueuePoller`. Subscribes to
// the slot manager's decoded `EventMessage` stream and mirrors every
// CCCP socket-level state transition into the Bifrost
// `cccp-relay-queue` pallet via `on_flight_poll` / `finalize_poll`
// extrinsics.
//
// Protocol recap (EVM-identical):
//   * `Requested` (status 1) on the source chain ⇒ submit
//     `on_flight_poll(msg, msg_hash, src_tx_id)`. BFC pallet records the
//     transfer in `PendingTransfers[msg_hash][src_tx_id]` until a
//     majority of relayers vote, then promotes it to
//     `OnFlightTransfers[msg_hash]`.
//   * `Committed` (7) or `Rollbacked` (8) ⇒ submit `finalize_poll(msg)`,
//     which moves the entry from `OnFlightTransfers` to
//     `FinalizedTransfers`.
//   * Storage lookups use `msg_hash = keccak256(encoded_msg_with_status=1)`
//     — the pallet always keys by the Requested-status hash so the
//     finalize vote has to reset the status byte before hashing.
//
// Solana-specific bits:
//   * We don't have a 32-byte transaction hash on Solana — tx ids are
//     64-byte ed25519 signatures encoded as base58 strings. To produce
//     a deterministic `src_tx_id: H256` that every relayer agrees on
//     without coordination, we feed the raw signature bytes through
//     `keccak256` and keep the full 32-byte digest. Every relayer
//     observing the same Solana tx derives the same hash; a maliciously
//     chosen tx id would still collide with the real one (= pre-image
//     resistance of keccak) so there is no vote-splitting vector.
//   * The EVM `Socket_Message` shape is rebuilt from the decoded
//     `sol::Event` — same mapping the inbound handler uses to submit
//     `submit_outbound_requests`. The two handlers are intentionally
//     kept in lockstep; any drift between them would make the BFC
//     pallet's `msg_hash` lookups miss.

use std::sync::Arc;

use std::str::FromStr;

use alloy::primitives::{Address, Bytes, ChainId, FixedBytes, U256, keccak256};
use eyre::Result;
use solana_sdk::signature::Signature;
use subxt::ext::subxt_core::utils::AccountId20;
use subxt::{OnlineClient, backend::legacy::LegacyRpcMethods};
use tokio::sync::broadcast::Receiver;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use alloy::{
	network::Network,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};

use br_primitives::{
	contracts::socket::Socket_Struct::{Instruction, RequestID, Socket_Message, Task_Params},
	eth::SocketEventStatus,
	sol::{Event, EventMessage, EventType},
	substrate::{CustomConfig, FinalizePollSubmission, OnFlightPollSubmission, bifrost_runtime},
	tx::{
		FinalizePollMetadata, OnFlightPollMetadata, XtRequestMessage, XtRequestMetadata,
		XtRequestSender,
	},
	utils::sub_display_format,
};

use crate::eth::EthClient;
use crate::sol::client::SolClient;

const SUB_LOG_TARGET: &str = "sol-queue-poller";

/// Status byte used for `msg_hash` lookups — the BFC pallet stores
/// every transfer under the hash of the Requested-status message.
const STATUS_REQUESTED: u8 = 1;

pub struct SolSocketQueuePoller<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	pub client: SolClient,
	/// Bifrost client — used to sign extrinsic payloads with the
	/// relayer's secp256k1 key.
	pub bfc_client: Arc<EthClient<F, P, N>>,
	sub_client: OnlineClient<CustomConfig>,
	sub_rpc: LegacyRpcMethods<CustomConfig>,
	xt_request_sender: Arc<XtRequestSender>,
	event_stream: BroadcastStream<EventMessage>,
}

impl<F, P, N: Network> SolSocketQueuePoller<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub fn new(
		client: SolClient,
		bfc_client: Arc<EthClient<F, P, N>>,
		sub_client: OnlineClient<CustomConfig>,
		sub_rpc: LegacyRpcMethods<CustomConfig>,
		xt_request_sender: Arc<XtRequestSender>,
		event_stream: Receiver<EventMessage>,
	) -> Self {
		Self {
			client,
			bfc_client,
			sub_client,
			sub_rpc,
			xt_request_sender,
			event_stream: BroadcastStream::new(event_stream),
		}
	}

	/// Rebuild the canonical EVM `Socket_Message` shape from a decoded
	/// Solana event. Kept byte-identical to
	/// `SolInboundHandler::build_socket_message` — the BFC pallet's
	/// `keccak256` lookup will miss if the two drift.
	fn build_socket_message(&self, ev: &Event) -> Socket_Message {
		Socket_Message {
			req_id: RequestID {
				ChainIndex: FixedBytes::from(ev.req_chain),
				round_id: ev.round_id,
				sequence: ev.sequence,
			},
			status: ev.status,
			ins_code: Instruction {
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

	/// Derive the deterministic 32-byte tx id from a Solana signature.
	///
	/// We hash the **raw signature bytes** (not the base58 string) so
	/// every relayer on every machine arrives at the same digest
	/// regardless of case normalization, padding, or leading-zero
	/// handling of the base58 representation.
	fn sol_signature_to_tx_id(signature: &str) -> subxt::utils::H256 {
		// Solana signatures are 64 bytes. Prefer decoding via
		// `solana_sdk::Signature` (same base58 parser the slot manager
		// already uses) so the hash is over canonical raw bytes; if
		// decoding fails (should never happen for a signature the slot
		// manager just accepted), fall back to hashing the string
		// bytes directly — still deterministic across relayers.
		let raw: Vec<u8> = Signature::from_str(signature)
			.map(|s| s.as_ref().to_vec())
			.unwrap_or_else(|_| signature.as_bytes().to_vec());
		subxt::utils::H256(keccak256(&raw).0)
	}

	/// Compute the msg_hash the BFC pallet uses to key storage maps.
	///
	/// Always hashes with `status = Requested` regardless of the
	/// message's actual phase — same trick the EVM poller plays with
	/// `requested_msg.status = 1` before hashing.
	fn compute_requested_msg_hash(&self, ev: &Event) -> subxt::utils::H256 {
		let mut requested = self.build_socket_message(ev);
		requested.status = STATUS_REQUESTED;
		let encoded: Vec<u8> = requested.into();
		subxt::utils::H256(keccak256(&encoded).0)
	}

	/// Query the pallet state to see if this relayer should skip the
	/// on-flight vote. Short-circuits when the transfer has already been
	/// approved, finalized, or voted on by this relayer.
	async fn should_skip_on_flight_poll(
		&self,
		msg_hash: subxt::utils::H256,
		src_tx_id: subxt::utils::H256,
	) -> Result<bool> {
		let best_hash = self.sub_rpc.chain_get_block_hash(None).await?.unwrap_or_default();
		let storage = self.sub_client.storage().at(best_hash);

		let on_flight = storage
			.fetch(&bifrost_runtime::storage().cccp_relay_queue().on_flight_transfers(msg_hash))
			.await?;
		if on_flight.is_some() {
			return Ok(true);
		}

		let finalized = storage
			.fetch(&bifrost_runtime::storage().cccp_relay_queue().finalized_transfers(msg_hash))
			.await?;
		if finalized.is_some() {
			return Ok(true);
		}

		let pending = storage
			.fetch(
				&bifrost_runtime::storage()
					.cccp_relay_queue()
					.pending_transfers(msg_hash, src_tx_id),
			)
			.await?;
		if let Some(pending) = pending {
			let voter = AccountId20(self.bfc_client.address().await.0.0);
			if pending.on_flight_voters.0.iter().any(|v| v == &voter) {
				return Ok(true);
			}
		}
		Ok(false)
	}

	async fn should_skip_finalize_poll(&self, msg_hash: subxt::utils::H256) -> Result<bool> {
		let best_hash = self.sub_rpc.chain_get_block_hash(None).await?.unwrap_or_default();
		let storage = self.sub_client.storage().at(best_hash);

		let finalized = storage
			.fetch(&bifrost_runtime::storage().cccp_relay_queue().finalized_transfers(msg_hash))
			.await?;
		if finalized.is_some() {
			return Ok(true);
		}

		let on_flight = storage
			.fetch(&bifrost_runtime::storage().cccp_relay_queue().on_flight_transfers(msg_hash))
			.await?;
		if let Some(on_flight) = on_flight {
			let voter = AccountId20(self.bfc_client.address().await.0.0);
			if on_flight.finalization_voters.0.iter().any(|v| v == &voter) {
				return Ok(true);
			}
		}
		Ok(false)
	}

	async fn process_event(&self, ev: &Event) -> Result<()> {
		let status = SocketEventStatus::from(ev.status);

		// We only care about three statuses: Requested, Committed,
		// Rollbacked. Anything else (Accepted etc.) is handled by the
		// inbound/outbound handlers and should be ignored here.
		match status {
			SocketEventStatus::Requested => self.process_requested(ev).await,
			SocketEventStatus::Committed | SocketEventStatus::Rollbacked => {
				self.process_finalized(ev).await
			},
			_ => Ok(()),
		}
	}

	async fn process_requested(&self, ev: &Event) -> Result<()> {
		let msg = self.build_socket_message(ev);
		let encoded_msg: Vec<u8> = msg.into();
		let msg_hash = subxt::utils::H256(keccak256(&encoded_msg).0);
		let src_tx_id = Self::sol_signature_to_tx_id(&ev.signature);
		let src_chain_id: ChainId = self.client.chain_id;
		let dst_chain_id: ChainId =
			Into::<u32>::into(FixedBytes::<4>::from(ev.ins_code_chain)) as ChainId;
		let is_inbound = dst_chain_id == self.bfc_client.chain_id();

		if self.should_skip_on_flight_poll(msg_hash, src_tx_id).await? {
			log::debug!(
				target: &self.client.get_chain_name(),
				"-[{}] skipping on-flight vote (already voted/finalized): sig={} msg_hash={}",
				sub_display_format(SUB_LOG_TARGET),
				ev.signature,
				msg_hash,
			);
			br_metrics::increase_sol_onflight_votes(&self.client.name, "skipped");
			return Ok(());
		}

		let metadata =
			OnFlightPollMetadata::new(is_inbound, ev.sequence, src_chain_id, dst_chain_id);

		self.submit_on_flight_poll(encoded_msg, msg_hash, src_tx_id, metadata).await
	}

	async fn process_finalized(&self, ev: &Event) -> Result<()> {
		let msg = self.build_socket_message(ev);
		let encoded_msg: Vec<u8> = msg.into();
		let requested_msg_hash = self.compute_requested_msg_hash(ev);
		let src_chain_id: ChainId = self.client.chain_id;
		let dst_chain_id: ChainId =
			Into::<u32>::into(FixedBytes::<4>::from(ev.ins_code_chain)) as ChainId;
		let is_inbound = dst_chain_id == self.bfc_client.chain_id();
		let is_committed =
			matches!(SocketEventStatus::from(ev.status), SocketEventStatus::Committed);

		if self.should_skip_finalize_poll(requested_msg_hash).await? {
			log::debug!(
				target: &self.client.get_chain_name(),
				"-[{}] skipping finalize vote (already voted/finalized): sig={} msg_hash={}",
				sub_display_format(SUB_LOG_TARGET),
				ev.signature,
				requested_msg_hash,
			);
			br_metrics::increase_sol_finalize_votes(&self.client.name, "skipped");
			return Ok(());
		}

		let metadata = FinalizePollMetadata::new(
			is_inbound,
			ev.sequence,
			src_chain_id,
			dst_chain_id,
			is_committed,
		);

		self.submit_finalize_poll(encoded_msg, metadata).await
	}

	async fn submit_on_flight_poll(
		&self,
		encoded_msg: Vec<u8>,
		msg_hash: subxt::utils::H256,
		src_tx_id: subxt::utils::H256,
		metadata: OnFlightPollMetadata,
	) -> Result<()> {
		use subxt::ext::codec::Encode;

		// Node expects the signing payload:
		//   keccak256("OnFlightPoll") || SCALE_encode((msg, msg_hash, src_tx_id))
		// — identical to the EVM poller, so BFC-side verification is
		// uniform across tracks.
		let prefix = keccak256("OnFlightPoll".as_bytes());
		let encoded_data = (&encoded_msg, &msg_hash, &src_tx_id).encode();
		let message_to_sign = [prefix.as_slice(), &encoded_data].concat();
		let signature = self
			.bfc_client
			.sign_message(&message_to_sign)
			.await
			.map_err(|e| eyre::eyre!("sign_message: {e}"))?
			.into();

		let call = Arc::new(bifrost_runtime::tx().cccp_relay_queue().on_flight_poll(
			OnFlightPollSubmission {
				authority_id: AccountId20(self.bfc_client.address().await.0.0),
				msg: encoded_msg.into(),
				msg_hash,
				src_tx_id,
			},
			signature,
		));

		match self
			.xt_request_sender
			.send(XtRequestMessage::new(call, XtRequestMetadata::OnFlightPoll(metadata.clone())))
		{
			Ok(_) => {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔖 Queued on-flight vote: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata,
				);
				br_metrics::increase_sol_onflight_votes(&self.client.name, "submitted");
			},
			Err(error) => {
				br_metrics::increase_sol_onflight_votes(&self.client.name, "failed");
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to queue on-flight vote: {}, Error: {}",
					metadata,
					error
				);
			},
		}
		Ok(())
	}

	async fn submit_finalize_poll(
		&self,
		encoded_msg: Vec<u8>,
		metadata: FinalizePollMetadata,
	) -> Result<()> {
		// Node expects raw bytes (NOT SCALE encoded), matching EVM:
		//   keccak256("FinalizePoll") || msg
		let prefix = keccak256("FinalizePoll".as_bytes());
		let message_to_sign = [prefix.as_slice(), &encoded_msg].concat();
		let signature = self
			.bfc_client
			.sign_message(&message_to_sign)
			.await
			.map_err(|e| eyre::eyre!("sign_message: {e}"))?
			.into();

		let call = Arc::new(bifrost_runtime::tx().cccp_relay_queue().finalize_poll(
			FinalizePollSubmission {
				authority_id: AccountId20(self.bfc_client.address().await.0.0),
				msg: encoded_msg.into(),
			},
			signature,
		));

		match self
			.xt_request_sender
			.send(XtRequestMessage::new(call, XtRequestMetadata::FinalizePoll(metadata.clone())))
		{
			Ok(_) => {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🏁 Queued finalize vote: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata,
				);
				br_metrics::increase_sol_finalize_votes(&self.client.name, "submitted");
			},
			Err(error) => {
				br_metrics::increase_sol_finalize_votes(&self.client.name, "failed");
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to queue finalize vote: {}, Error: {}",
					metadata,
					error
				);
			},
		}
		Ok(())
	}

	pub async fn run(&mut self) -> Result<()> {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] sol queue poller started",
			sub_display_format(SUB_LOG_TARGET),
		);

		while let Some(item) = self.event_stream.next().await {
			let msg = match item {
				Ok(m) => m,
				Err(err) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] event stream lag/error: {err:?}",
						sub_display_format(SUB_LOG_TARGET),
					);
					continue;
				},
			};

			// Only socket events carry state transitions we care about;
			// NewSlot heartbeats pass through without action.
			if matches!(msg.event_type, EventType::NewSlot) {
				continue;
			}

			for ev in &msg.events {
				if let Err(err) = self.process_event(ev).await {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] process_event(sig={}) failed: {err:?}",
						sub_display_format(SUB_LOG_TARGET),
						ev.signature,
					);
				}
			}
		}
		Ok(())
	}
}
