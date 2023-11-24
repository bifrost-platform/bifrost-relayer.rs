use crate::eth::{EthClient, EventMessage, EventMetadata, FlushMetadata, TxRequest};
use async_trait::async_trait;
use br_primitives::eth::GasCoefficient;
use ethers::{providers::JsonRpcClient, types::Transaction};
use rand::Rng;
use sc_service::SpawnTaskHandle;
use std::sync::Arc;

mod eip1559_manager;
mod legacy_manager;
mod task;

pub use eip1559_manager::*;
pub use legacy_manager::*;
pub use task::*;

/// Generates a random delay that is ranged as 0 to 12000 milliseconds (in milliseconds).
pub fn generate_delay() -> u64 {
	rand::thread_rng().gen_range(0..=12000)
}

#[async_trait]
/// The manager trait for Legacy and Eip1559 transactions.
pub trait TransactionManager<T>
where
	T: JsonRpcClient,
{
	/// Starts the transaction manager. Listens to every new consumed event message.
	async fn run(&mut self);

	/// Initialize transaction manager.
	async fn initialize(&mut self);

	/// Get the `EthClient`.
	fn get_client(&self) -> Arc<EthClient<T>>;

	/// Get the transaction spawn handle.
	fn get_spawn_handle(&self) -> SpawnTaskHandle;

	/// Spawn a transaction task and try sending the transaction.
	async fn spawn_send_transaction(&self, msg: EventMessage);

	/// The flag whether the client has enabled txpool namespace.
	fn is_txpool_enabled(&self) -> bool;

	/// Flush all transaction from mempool.
	async fn flush_stuck_transaction(&self) {
		if self.is_txpool_enabled() && !self.get_client().metadata.is_native {
			let mempool = self.get_client().get_txpool_content().await;
			br_metrics::increase_rpc_calls(&self.get_client().get_chain_name());

			let mut transactions = Vec::new();
			transactions.extend(
				mempool.queued.get(&self.get_client().address()).cloned().unwrap_or_default(),
			);
			transactions.extend(
				mempool.pending.get(&self.get_client().address()).cloned().unwrap_or_default(),
			);

			for (_nonce, transaction) in transactions {
				self.spawn_send_transaction(EventMessage::new(
					self.stuck_transaction_to_transaction_request(&transaction).await,
					EventMetadata::Flush(FlushMetadata::default()),
					false,
					false,
					GasCoefficient::Low,
				))
				.await;
			}
		}
	}

	/// Converts stuck transaction to `TxRequest(TransactionRequest | Eip1559TransactionRequest)`
	async fn stuck_transaction_to_transaction_request(
		&self,
		transaction: &Transaction,
	) -> TxRequest;
}
