use ethers::types::TransactionRequest;

#[async_trait::async_trait]
pub trait OffchainWorker {
	/// Starts the offchain worker. Do what it have to do according to a fixed schedule.
	async fn run(&mut self);

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(&self, dst_chain_id: u32, transaction: TransactionRequest);
}

#[async_trait::async_trait]
pub trait TimeDrivenOffchainWorker {
	/// Wait until next schedule
	async fn wait_until_next_time();
}
