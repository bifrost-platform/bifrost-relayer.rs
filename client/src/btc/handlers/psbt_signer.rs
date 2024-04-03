use br_primitives::{
	constants::errors::INVALID_PROVIDER_URL,
	sub_display_format,
	substrate::{
		bifrost_runtime, AccountId20, CustomConfig, EthereumSignature, Signature,
		SignedPsbtMessage, SubmitSignedPsbt,
	},
	tx::{SubmitSignedPsbtMetadata, XtRequestMessage, XtRequestMetadata, XtRequestSender},
};
use ethers::{
	providers::JsonRpcClient,
	types::{Bytes, Signature as EthersSignature},
};
use miniscript::bitcoin::Psbt;
use subxt::{tx::Payload, OnlineClient};
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
	/// The substrate client.
	sub_client: Option<OnlineClient<CustomConfig>>,
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
			sub_client: None,
			bfc_client,
			xt_request_sender,
			event_receiver,
			target_event: EventType::NewBlock,
			keypair_storage,
		}
	}

	async fn initialize(&mut self) {
		self.sub_client = Some(
			OnlineClient::<CustomConfig>::from_url(&self.bfc_client.metadata.url)
				.await
				.expect(INVALID_PROVIDER_URL),
		);
	}

	async fn get_unsigned_psbts(&self) -> Vec<Bytes> {
		let socket_queue = self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap();

		self.bfc_client
			.contract_call(socket_queue.unsigned_psbts(), "socket_queue.unsigned_psbts")
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
			let signature = self.convert_ethers_to_ecdsa_signature(
				self.bfc_client.wallet.sign_message(&signed_psbt),
			);
			return Some((msg, signature));
		}
		None
	}

	fn build_unsigned_tx(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Option<XtRequestMessage<Payload<SubmitSignedPsbt>>> {
		if let Some((msg, signature)) = self.build_payload(unsigned_psbt) {
			let call = bifrost_runtime::tx().btc_socket_queue().submit_signed_psbt(msg, signature);
			return Some(XtRequestMessage::new(
				call,
				XtRequestMetadata::SubmitSignedPsbt(SubmitSignedPsbtMetadata {}),
			));
		}
		None
	}

	fn convert_ethers_to_ecdsa_signature(
		&self,
		ethers_signature: EthersSignature,
	) -> EthereumSignature {
		let sig: String = format!("0x{}", ethers_signature);

		let bytes = sig.as_bytes();

		let mut decode_sig = [0u8; 65];
		decode_sig.copy_from_slice(bytes);

		EthereumSignature(Signature(decode_sig))
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for PsbtSigner<T> {
	async fn run(&mut self) {
		self.initialize().await;

		loop {
			let msg = self.event_receiver.recv().await.unwrap();

			if !self.is_target_event(msg.event_type) {
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
				if let Some(tx) =
					self.build_unsigned_tx(&mut Psbt::deserialize(&unsigned_psbt).unwrap())
				{
					match self.xt_request_sender.send(tx) {
						Ok(_) => todo!(),
						Err(_) => todo!(),
					}
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
