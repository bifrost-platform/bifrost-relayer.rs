use br_primitives::{
	substrate::{
		bifrost_runtime, AccountId20, EthereumSignature, SignedPsbtMessage, SubmitSignedPsbt,
	},
	tx::{SubmitSignedPsbtMetadata, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::{convert_ethers_to_ecdsa_signature, hash_bytes, sub_display_format},
};
use ethers::{providers::JsonRpcClient, types::Bytes};
use miniscript::bitcoin::Psbt;
use subxt::tx::Payload;
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use std::sync::Arc;

use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
		storage::keypair::KeypairStorage,
	},
	eth::EthClient,
};

const SUB_LOG_TARGET: &str = "psbt-signer";

pub struct PsbtSigner<T> {
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T>>,
	xt_request_sender: Arc<XtRequestSender<Payload<SubmitSignedPsbt>>>,
	event_receiver: Receiver<BTCEventMessage>,
	target_event: EventType,
	keypair_storage: KeypairStorage,
}

impl<T: JsonRpcClient> PsbtSigner<T> {
	pub fn new(
		bfc_client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender<Payload<SubmitSignedPsbt>>>,
		event_receiver: Receiver<BTCEventMessage>,
		keypair_storage: KeypairStorage,
	) -> Self {
		Self {
			bfc_client,
			xt_request_sender,
			event_receiver,
			target_event: EventType::NewBlock,
			keypair_storage,
		}
	}

	async fn get_unsigned_psbts(&self) -> Vec<Bytes> {
		let socket_queue = self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap();

		self.bfc_client
			.contract_call(socket_queue.unsigned_psbts(), "socket_queue.unsigned_psbts")
			.await
	}

	async fn is_relay_executive(&self) -> bool {
		let relay_exec = self.bfc_client.protocol_contracts.relay_executive.as_ref().unwrap();

		self.bfc_client
			.contract_call(
				relay_exec.is_member(self.bfc_client.address()),
				"relay_executive.is_member",
			)
			.await
	}

	fn build_payload(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Option<(SignedPsbtMessage<AccountId20>, EthereumSignature)> {
		let mut psbt = unsigned_psbt.clone();
		if self.keypair_storage.sign_psbt(&mut psbt) {
			let signed_psbt = psbt.serialize();
			let msg = SignedPsbtMessage {
				authority_id: AccountId20(self.bfc_client.address().0),
				unsigned_psbt: unsigned_psbt.serialize(),
				signed_psbt: signed_psbt.clone(),
			};
			let signature = convert_ethers_to_ecdsa_signature(
				self.bfc_client.wallet.sign_message(&signed_psbt),
			);
			return Some((msg, signature));
		}
		None
	}

	fn build_unsigned_tx(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Option<(Payload<SubmitSignedPsbt>, SubmitSignedPsbtMetadata)> {
		if let Some((msg, signature)) = self.build_payload(unsigned_psbt) {
			let metadata = SubmitSignedPsbtMetadata::new(
				hash_bytes(&msg.unsigned_psbt),
				hash_bytes(&msg.signed_psbt),
			);
			return Some((
				bifrost_runtime::tx().btc_socket_queue().submit_signed_psbt(msg, signature),
				metadata,
			));
		}
		None
	}

	fn request_send_transaction(
		&self,
		call: Payload<SubmitSignedPsbt>,
		metadata: SubmitSignedPsbtMetadata,
	) {
		match self.xt_request_sender.send(XtRequestMessage::new(
			call,
			XtRequestMetadata::SubmitSignedPsbt(metadata.clone()),
		)) {
			Ok(_) => log::info!(
				target: LOG_TARGET,
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				log::error!(
					target: LOG_TARGET,
					"-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
						LOG_TARGET,
						SUB_LOG_TARGET,
						self.bfc_client.address(),
						metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
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
				target: LOG_TARGET,
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
					self.request_send_transaction(call, metadata);
				}
			}
		}
	}

	async fn process_event(&self, _event_tx: Event, _is_bootstrap: bool) {}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}