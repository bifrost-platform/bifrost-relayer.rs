use ethers::types::{Log, H256};

use br_primitives::eth::BootstrapState;
pub use bridge_relay_handler::*;
pub use roundup_relay_handler::*;

mod bridge_relay_handler;
mod roundup_relay_handler;

#[async_trait::async_trait]
pub trait Handler {
	/// Starts the event handler and listens to every new consumed block.
	async fn run(&mut self);

	/// Decode and parse the event if the given transaction triggered an event.
	async fn process_confirmed_transaction(&self, log: &Log, is_bootstrap: bool);

	/// Verifies whether the given transaction interacted with the target contract.
	fn is_target_contract(&self, log: &Log) -> bool;

	/// Verifies whether the given event topic matches the target event signature.
	fn is_target_event(&self, topic: H256) -> bool;
}

#[async_trait::async_trait]
pub trait BootstrapHandler {
	/// Starts the bootstrap process.
	async fn bootstrap(&self);

	/// Fetch the historical events to bootstrap.
	async fn get_bootstrap_events(&self) -> Vec<Log>;

	/// Verifies whether the bootstrap state has been synced to the given state.
	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool;
}
