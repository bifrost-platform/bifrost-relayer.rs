mod cccp_handler;

use ethers::types::{TransactionReceipt, TransactionRequest, H256};
pub use cccp_handler::*;

#[async_trait::async_trait]
pub trait Handler {
	async fn run(&self);

	/// Decode and parse the event if the given transaction triggered an event.
	async fn process_confirmed_transaction(&self, receipt: TransactionReceipt);

	/// Request send relay transaction to `dst_chain_id` channel.
	async fn request_send_transaction(&self, dst_chain_id: u32, transaction: TransactionRequest);

	/// Verifies whether the given transaction interacted with the target contract.
	fn is_target_contract(&self, receipt: &TransactionReceipt) -> bool;

	/// Verifies whether the given event topic matches the target event signature.
	fn is_target_event(topic: H256) -> bool;
}
