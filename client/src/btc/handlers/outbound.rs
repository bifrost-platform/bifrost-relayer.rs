use crate::btc::{
	block::{Event, EventMessage as BTCEventMessage, EventType},
	handlers::Handler,
};
use br_primitives::tx::TxRequestSender;
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;

pub struct OutboundHandler {
	tx_request_sender: Arc<TxRequestSender>,
	event_receiver: Receiver<BTCEventMessage>,
	target_event: EventType,
}

impl OutboundHandler {
	pub fn new(
		tx_request_sender: Arc<TxRequestSender>,
		event_receiver: Receiver<BTCEventMessage>,
	) -> Self {
		Self { tx_request_sender, event_receiver, target_event: EventType::Outbound }
	}
}

#[async_trait::async_trait]
impl Handler for OutboundHandler {
	async fn run(&mut self) {
		todo!()
	}

	async fn process_event(&self, event_tx: Event, is_bootstrap: bool) {
		todo!()
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}
