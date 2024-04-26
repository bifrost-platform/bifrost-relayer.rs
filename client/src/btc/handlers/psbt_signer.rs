use br_primitives::{
	substrate::{bifrost_runtime, AccountId20, EthereumSignature, SignedPsbtMessage},
	tx::{SubmitSignedPsbtMetadata, XtRequest, XtRequestMetadata, XtRequestSender},
	utils::{convert_ethers_to_ecdsa_signature, hash_bytes, sub_display_format},
};
use ethers::{providers::JsonRpcClient, types::Bytes};
use miniscript::bitcoin::Psbt;
use std::collections::BTreeSet;
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use std::sync::Arc;

use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::Handler,
		storage::keypair::KeypairStorage,
	},
	eth::EthClient,
};

use super::XtRequester;

const SUB_LOG_TARGET: &str = "psbt-signer";

/// The essential task that submits signed PSBT's.
pub struct PsbtSigner<T> {
	/// The Bifrost client.
	client: Arc<EthClient<T>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The Bitcoin event receiver.
	event_receiver: Receiver<BTCEventMessage>,
	/// The target Bitcoin event.
	target_event: EventType,
	/// The public and private keypair local storage.
	keypair_storage: KeypairStorage,
}

impl<T: JsonRpcClient> PsbtSigner<T> {
	/// Instantiates a new `PsbtSigner` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender>,
		event_receiver: Receiver<BTCEventMessage>,
		keypair_storage: KeypairStorage,
	) -> Self {
		Self {
			client,
			xt_request_sender,
			event_receiver,
			target_event: EventType::NewBlock,
			keypair_storage,
		}
	}

	/// Get the pending unsigned PSBT's (in bytes)
	async fn get_unsigned_psbts(&self) -> Vec<Bytes> {
		let socket_queue = self.client.protocol_contracts.socket_queue.as_ref().unwrap();

		self.client
			.contract_call(socket_queue.unsigned_psbts(), "socket_queue.unsigned_psbts")
			.await
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> bool {
		let relay_exec = self.client.protocol_contracts.relay_executive.as_ref().unwrap();

		self.client
			.contract_call(relay_exec.is_member(self.client.address()), "relay_executive.is_member")
			.await
	}

	/// Build the payload for the unsigned transaction. (`submit_signed_psbt()`)
	fn build_payload(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Option<(SignedPsbtMessage<AccountId20>, EthereumSignature)> {
		let mut psbt = unsigned_psbt.clone();
		if self.keypair_storage.sign_psbt(&mut psbt) {
			let signed_psbt = psbt.serialize();
			let msg = SignedPsbtMessage {
				authority_id: AccountId20(self.client.address().0),
				unsigned_psbt: unsigned_psbt.serialize(),
				signed_psbt: signed_psbt.clone(),
			};
			let signature = convert_ethers_to_ecdsa_signature(
				self.client.wallet.sign_message(signed_psbt.as_ref()),
			);
			return Some((msg, signature));
		}
		log::warn!(
			target: &self.client.get_chain_name(),
			"-[{}] 🔐 Unauthorized to sign PSBT: {}",
			sub_display_format(SUB_LOG_TARGET),
			hash_bytes(&psbt.serialize())
		);
		None
	}

	/// Build the calldata for the unsigned transaction. (`submit_signed_psbt()`)
	fn build_unsigned_tx(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Option<(XtRequest, SubmitSignedPsbtMetadata)> {
		if let Some((msg, signature)) = self.build_payload(unsigned_psbt) {
			let metadata = SubmitSignedPsbtMetadata::new(hash_bytes(&msg.unsigned_psbt));
			return Some((
				XtRequest::from(
					bifrost_runtime::tx().btc_socket_queue().submit_signed_psbt(msg, signature),
				),
				metadata,
			));
		}
		None
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> XtRequester<T> for PsbtSigner<T> {
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for PsbtSigner<T> {
	async fn run(&mut self) {
		loop {
			let msg = self.event_receiver.recv().await.unwrap();

			if !self.is_target_event(msg.event_type) {
				continue;
			}
			if !self.is_relay_executive().await {
				continue;
			}

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 📦 Imported #{:?} with target logs({:?})",
				sub_display_format(SUB_LOG_TARGET),
				msg.block_number,
				msg.events.len()
			);

			let unsigned_psbts = self.get_unsigned_psbts().await;
			let mut stream = tokio_stream::iter(unsigned_psbts);
			while let Some(unsigned_psbt) = stream.next().await {
				if let Some((call, metadata)) =
					self.build_unsigned_tx(&mut Psbt::deserialize(&unsigned_psbt).unwrap())
				{
					self.request_send_transaction(
						call,
						XtRequestMetadata::SubmitSignedPsbt(metadata),
						SUB_LOG_TARGET,
					);
				}
			}
		}
	}

	async fn process_event(&self, _event_tx: Event, _: &mut BTreeSet<Bytes>, _is_bootstrap: bool) {}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}
