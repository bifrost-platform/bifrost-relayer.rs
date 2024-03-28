mod inbound;
mod outbound;

pub use inbound::*;
pub use outbound::*;

use crate::btc::block::{Event, EventType};
use br_primitives::eth::BootstrapState;
use miniscript::bitcoin::Transaction;

pub const LOG_TARGET: &str = "Bitcoin";

#[async_trait::async_trait]
pub trait Handler {
	async fn run(&mut self);

	async fn process_event(&self, event_tx: Event, is_bootstrap: bool);

	fn is_target_event(&self, event_type: EventType) -> bool;
}

#[async_trait::async_trait]
pub trait BootstrapHandler {
	async fn bootstrap(&self);

	async fn get_bootstrap_events(&self) -> Vec<Transaction>;

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool;
}
