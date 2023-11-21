use ethers::types::{Log, H256};

pub use roundup_relay_handler::*;
pub use socket_relay_handler::*;

mod roundup_relay_handler;
mod socket_relay_handler;
pub mod v2;

#[async_trait::async_trait]
pub trait Handler {
	/// Starts the event handler and listens to every new consumed block.
	async fn run(&mut self);

	/// Decode and parse the event if the given log triggered an relay target event.
	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool);

	/// Verifies whether the given transaction interacted with the target contract.
	fn is_target_contract(&self, log: &Log) -> bool;

	/// Verifies whether the given event topic matches the target event signature.
	fn is_target_event(&self, topic: H256) -> bool;
}
