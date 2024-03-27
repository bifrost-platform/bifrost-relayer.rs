use crate::btc::block::{Event, EventMessage as BTCEventMessage, EventType};
use crate::btc::handlers::Handler;
use crate::btc::storage::pending_outbound::PendingOutboundPool;
use br_primitives::periodic::RollbackSender;
use br_primitives::tx::TxRequestSender;
use miniscript::bitcoin::address::NetworkUnchecked;
use miniscript::bitcoin::Address;
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;

pub struct OutboundHandler {
	tx_request_sender: Arc<TxRequestSender>,
	rollback_sender: Arc<RollbackSender>,
	event_receiver: Receiver<BTCEventMessage>,
	pending_outbound: PendingOutboundPool,
	target_event: EventType,
	change_address: Address<NetworkUnchecked>,
}

impl OutboundHandler {
	fn new(
		tx_request_sender: Arc<TxRequestSender>,
		rollback_sender: Arc<RollbackSender>,
		event_receiver: Receiver<BTCEventMessage>,
		pending_outbound: PendingOutboundPool,
		change_address: Address,
	) -> Self {
		Self {
			tx_request_sender,
			rollback_sender,
			event_receiver,
			pending_outbound,
			target_event: EventType::Outbound,
			change_address,
		}
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
