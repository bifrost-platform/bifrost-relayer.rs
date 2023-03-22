mod socket_handler;

use ethers::types::{Transaction, TransactionReceipt, H256};
pub use socket_handler::*;

#[async_trait::async_trait]
pub trait Handler {
	async fn run(&self);

	async fn process_confirmed_transaction(&self, receipt: TransactionReceipt);

	async fn send_transaction(&self, transaction: Transaction);

	fn is_target_contract(&self, receipt: TransactionReceipt) -> bool;

	fn is_target_event(topic: H256) -> bool;
}
