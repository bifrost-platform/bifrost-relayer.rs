// SPDX-License-Identifier: Apache-2.0
//
// Inbound handler. Subscribes to the slot manager's `EventMessage`
// channel; for every confirmed `Inbound` event it:
//
//   1. Reconstructs the EVM `Socket_Message` shape from the on-chain
//      `SocketEvent` (the cccp-solana program emits a struct that mirrors
//      the EVM one byte-for-byte; see
//      `cccp-solana/programs/cccp-solana/src/codec/named_type.rs` and the
//      cross-impl tests in `crate::sol::codec`).
//   2. ABI-encodes the message via the existing
//      `From<Socket_Message> for Vec<u8>` implementation in
//      `br_primitives::contracts::socket`. This is the same byte sequence
//      the EVM-side `submit_brp_outbound_request` produces — Bifrost has
//      no way to tell the difference once it's on the wire.
//   3. Wraps it in `pallet_blaze::SocketMessagesSubmission` and signs the
//      payload with the relayer's secp256k1 key (`EthClient::sign_message`).
//   4. Pushes the resulting unsigned extrinsic onto `XtRequestSender` so
//      the substrate side picks it up.
//
// The flow mirrors `SocketRelayHandler::submit_brp_outbound_request` for
// the BTC track. The biggest practical difference is that the cccp-solana
// program already encodes the relayer-friendly fields into a clean event,
// so we don't need to re-derive `req_id` from a Bitcoin txid or anything
// chain-specific.

use std::sync::Arc;

use alloy::{
	network::Network,
	primitives::{Address, Bytes, FixedBytes, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use array_bytes::Hexify;
use subxt::ext::subxt_core::utils::AccountId20;
use tokio::sync::broadcast::Receiver;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use br_primitives::{
	contracts::socket::Socket_Struct::{Instruction, RequestID, Socket_Message, Task_Params},
	eth::SocketEventStatus,
	sol::{Event, EventMessage, EventType},
	substrate::{SocketMessagesSubmission, bifrost_runtime},
	tx::{SocketRelayMetadata, XtRequest, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};

use crate::eth::EthClient;
use crate::sol::client::SolClient;

const SUB_LOG_TARGET: &str = "sol-inbound";

pub struct SolInboundHandler<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub client: SolClient,
	/// Bifrost client (used to sign messages with the relayer's secp256k1
	/// key + look up the relayer address). Mirror of the BTC inbound
	/// handler's `bfc_client` field.
	pub bfc_client: Arc<EthClient<F, P, N>>,
	xt_request_sender: Arc<XtRequestSender>,
	event_stream: BroadcastStream<EventMessage>,
}

impl<F, P, N: Network> SolInboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub fn new(
		client: SolClient,
		bfc_client: Arc<EthClient<F, P, N>>,
		xt_request_sender: Arc<XtRequestSender>,
		event_stream: Receiver<EventMessage>,
	) -> Self {
		Self {
			client,
			bfc_client,
			xt_request_sender,
			event_stream: BroadcastStream::new(event_stream),
		}
	}

	/// Convert a decoded Solana `Event` into the EVM `Socket_Struct::Socket_Message`
	/// shape. The mapping is mechanical: every field of the on-chain
	/// `SocketEvent` (= cccp-solana `SocketMessage`) has a 1:1 EVM
	/// counterpart. The cross-impl byte fixture in
	/// `crate::sol::codec::tests::cross_impl_request_id_pack` and the
	/// matching test on the cccp-solana side guarantee that the encoded
	/// hash matches what BFC's relayer set already signed.
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

	/// Build the unsigned `submit_outbound_requests` extrinsic for one
	/// Solana inbound event. The signature payload is `keccak256(hex(...))`
	/// (`hexify_prefixed`) — same convention used by the BTC track's
	/// `SocketRelayHandler::submit_brp_outbound_request`.
	async fn build_unsigned_tx(
		&self,
		ev: &Event,
	) -> eyre::Result<(XtRequest, SocketRelayMetadata)> {
		let socket_msg = self.build_socket_message(ev);
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

		let metadata = SocketRelayMetadata::new(
			true, // is_inbound — Solana → Bifrost
			SocketEventStatus::from(ev.status),
			ev.sequence,
			self.client.chain_id, // src = this Solana cluster
			// dst = Bifrost (BFC_MAIN). We pull it from the bfc_client's
			// chain_id metadata so testnet vs mainnet picks up the right
			// value automatically.
			self.bfc_client.chain_id(),
			Address::from(ev.to),
			false,
		);

		Ok((call, metadata))
	}

	async fn handle_event(&self, ev: &Event) {
		// Skip if the relayer wasn't selected for this round. The on-chain
		// `submit_outbound_requests` extrinsic will reject non-selected
		// relayers anyway, but pre-checking saves an RPC roundtrip per
		// Solana event.
		match self.bfc_client.is_selected_relayer().await {
			Ok(true) => {},
			Ok(false) => {
				log::debug!(
					target: &self.client.get_chain_name(),
					"-[{}] not a selected relayer this round; skipping {} (seq={})",
					sub_display_format(SUB_LOG_TARGET),
					ev.signature,
					ev.sequence,
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

		let (call, metadata) = match self.build_unsigned_tx(ev).await {
			Ok(x) => x,
			Err(err) => {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to build inbound extrinsic for sig={} seq={}: {err}",
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
				"-[{}] 🔖 Request unsigned tx (sol→bfc): {} sig={}",
				sub_display_format(SUB_LOG_TARGET),
				metadata,
				ev.signature,
			),
			Err(error) => {
				br_primitives::log_and_capture!(
					error,
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to send unsigned tx for {}: {error}",
					metadata
				);
			},
		}
	}

	pub async fn run(&mut self) -> eyre::Result<()> {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] sol inbound handler started",
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

			if msg.event_type != EventType::Inbound {
				continue;
			}

			for ev in &msg.events {
				self.handle_event(ev).await;
			}
		}

		Ok(())
	}
}
