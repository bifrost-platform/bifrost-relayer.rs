use br_primitives::{
	constants::errors::INVALID_PROVIDER_URL, sub_display_format, substrate::CustomConfig,
	tx::XtRequestSender,
};
use ethers::providers::JsonRpcClient;
use subxt::{tx::TxPayload, OnlineClient};
use tokio::sync::broadcast::Receiver;

use std::sync::Arc;

use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::EthClient,
};

const SUB_LOG_TARGET: &str = "psbt-signer";

pub struct PsbtSigner<T, Call> {
	/// The substrate client.
	sub_client: Option<OnlineClient<CustomConfig>>,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T>>,
	xt_request_sender: Arc<XtRequestSender<Call>>,
	event_receiver: Receiver<BTCEventMessage>,
	target_event: EventType,
}

impl<T: JsonRpcClient, Call: TxPayload> PsbtSigner<T, Call> {
	pub fn new(
		bfc_client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender<Call>>,
		event_receiver: Receiver<BTCEventMessage>,
	) -> Self {
		Self {
			sub_client: None,
			bfc_client,
			xt_request_sender,
			event_receiver,
			target_event: EventType::NewBlock,
		}
	}

	async fn initialize(&mut self) {
		self.sub_client = Some(
			OnlineClient::<CustomConfig>::from_url(&self.bfc_client.metadata.url)
				.await
				.expect(INVALID_PROVIDER_URL),
		);
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient, Call: TxPayload + Send> Handler for PsbtSigner<T, Call> {
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
		}
	}

	async fn process_event(&self, _event_tx: Event, _is_bootstrap: bool) {}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}
