mod bridge_relay_handler;
mod vsp_handler;

pub use bridge_relay_handler::*;
pub use vsp_handler::*;

use ethers::types::{TransactionReceipt, TransactionRequest, H256};

use super::RelayMetadata;

#[async_trait::async_trait]
pub trait Handler {
	/// Starts the event handler and listens to every new consumed block.
	async fn run(&mut self);

	/// Decode and parse the event if the given transaction triggered an event.
	async fn process_confirmed_transaction(&self, receipt: TransactionReceipt);

	/// Request send relay transaction to the target event channel.
	fn request_send_transaction(
		&self,
		chain_id: u32,
		tx_request: TransactionRequest,
		metadata: RelayMetadata,
	);

	/// Verifies whether the given transaction interacted with the target contract.
	fn is_target_contract(&self, receipt: &TransactionReceipt) -> bool;

	/// Verifies whether the given event topic matches the target event signature.
	fn is_target_event(topic: H256) -> bool;
}
