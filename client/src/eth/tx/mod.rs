use crate::eth::{EthClient, EventMessage, EventMetadata, FlushMetadata, TxRequest};
use async_trait::async_trait;
use br_primitives::{eth::GasCoefficient, sub_display_format};
use ethers::{
	providers::JsonRpcClient,
	types::{Transaction, TransactionReceipt, U256},
};
use rand::Rng;
use std::{error::Error, sync::Arc, time::Duration};
use tokio::time::sleep;

mod eip1559_manager;
mod legacy_manager;

pub use eip1559_manager::*;
pub use legacy_manager::*;

/// Generates a random delay that is ranged as 0 to 12000 milliseconds (in milliseconds).
pub fn generate_delay() -> u64 {
	rand::thread_rng().gen_range(0..=12000)
}

#[async_trait]
pub trait TransactionManager<T>
where
	T: JsonRpcClient,
{
	/// The flag whether the client has enabled txpool namespace.
	fn is_txpool_enabled(&self) -> bool;

	/// Verifies whether the relayer has sufficient funds to pay for the transaction.
	async fn is_sufficient_funds(&self, gas_price: U256, gas: U256) -> bool {
		let fee = gas_price.saturating_mul(gas);
		let client = self.get_client();
		let balance = client.get_balance(client.address()).await;
		br_metrics::increase_rpc_calls(&self.get_client().get_chain_name());
		if balance < fee {
			return false
		}
		true
	}

	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged such as
	/// gas estimation failures.
	fn debug_mode(&self) -> bool;

	/// Get the `EthClient`.
	fn get_client(&self) -> Arc<EthClient<T>>;

	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	fn duplicate_confirm_delay(&self) -> Duration;

	/// Initialize transaction manager.
	async fn initialize(&mut self);

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
				self.try_send_transaction(EventMessage::new(
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

	/// Starts the transaction manager. Listens to every new consumed event message.
	async fn run(&mut self);

	/// Retry send_transaction() for failed transaction execution.
	async fn retry_transaction(&self, mut msg: EventMessage) {
		msg.build_retry_event();
		sleep(Duration::from_millis(msg.retry_interval)).await;
		self.try_send_transaction(msg).await;
	}

	/// Sends the consumed transaction request to the connected chain. The transaction send will
	/// be retry if the transaction fails to be mined in a block.
	async fn try_send_transaction(&self, msg: EventMessage);

	/// Function that query mempool for check if the relay event that this relayer is about to send
	/// has already been processed by another relayer.
	async fn is_duplicate_relay(&self, tx_request: &TxRequest, check_mempool: bool) -> bool {
		let client = self.get_client();

		// does not check the txpool if the following condition satisfies
		// 1. the txpool namespace is disabled for the client
		// 2. the txpool check flag is false
		// 3. the client is BIFROST (native)
		if !self.is_txpool_enabled() || !check_mempool || client.metadata.is_native {
			return false
		}

		let (data, to, from) = (
			tx_request.get_data(),
			tx_request.get_to().as_address().unwrap(),
			tx_request.get_from(),
		);

		let mempool = self.get_client().get_txpool_content().await;
		br_metrics::increase_rpc_calls(&client.get_chain_name());

		for (_address, tx_map) in mempool.pending.iter().chain(mempool.queued.iter()) {
			for (_nonce, mempool_tx) in tx_map.iter() {
				if mempool_tx.to.unwrap_or_default() == *to && mempool_tx.input == *data {
					// Trying gas escalating is not duplicate action
					if mempool_tx.from == *from {
						return false
					}

					sleep(self.duplicate_confirm_delay()).await;
					return if self
						.get_client()
						.get_transaction_receipt(mempool_tx.hash)
						.await
						.is_some()
					{
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						true
					} else {
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						false
					}
				}
			}
		}
		false
	}

	/// Handles the successful transaction receipt.
	fn handle_success_tx_receipt(
		&self,
		sub_target: &str,
		receipt: Option<TransactionReceipt>,
		metadata: EventMetadata,
	) {
		let client = self.get_client();

		if let Some(receipt) = receipt {
			let status = receipt.status.unwrap();
			log::info!(
				target: &client.get_chain_name(),
				"-[{}] 🎁 The requested transaction has been successfully mined in block: {}, {:?}-{:?}-{:?}",
				sub_display_format(sub_target),
				metadata.to_string(),
				receipt.block_number.unwrap(),
				status,
				receipt.transaction_hash
			);
			if status.is_zero() && self.debug_mode() {
				log::warn!(
					target: &client.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during contract execution [execution reverted]. A prior transaction might have been already submitted: {}, {:?}-{:?}-{:?}",
					sub_display_format(sub_target),
					metadata,
					receipt.block_number.unwrap(),
					status,
					receipt.transaction_hash
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during contract execution [execution reverted]. A prior transaction might have been already submitted: {}, {:?}-{:?}-{:?}",
						&client.get_chain_name(),
						sub_target,
						client.address(),
						metadata,
						receipt.block_number.unwrap(),
						status,
						receipt.transaction_hash
					)
						.as_str(),
					sentry::Level::Warning,
				);
			}
			br_metrics::set_payed_fees(&client.get_chain_name(), &receipt);
		} else {
			log::info!(
				target: &client.get_chain_name(),
				"-[{}] 🎁 The pending transaction has been successfully replaced and gas-escalated: {}",
				sub_display_format(sub_target),
				metadata,
			);
		}
	}

	/// Handles the failed transaction receipt generation.
	async fn handle_failed_tx_receipt(&self, sub_target: &str, msg: EventMessage) {
		let client = self.get_client();

		log::error!(
			target: &client.get_chain_name(),
			"-[{}] ♻️  The requested transaction failed to generate a receipt: {}, Retries left: {:?}",
			sub_display_format(sub_target),
			msg.metadata,
			msg.retries_remaining - 1,
		);
		sentry::capture_message(
			format!(
				"[{}]-[{}]-[{}] ♻️  The requested transaction failed to generate a receipt: {}, Retries left: {:?}",
				&client.get_chain_name(),
				sub_target,
				client.address(),
				msg.metadata,
				msg.retries_remaining - 1,
			)
			.as_str(),
			sentry::Level::Error,
		);
		self.retry_transaction(msg).await;
	}

	/// Handles the failed transaction request.
	async fn handle_failed_tx_request<E: Error + Sync + ?Sized>(
		&self,
		sub_target: &str,
		msg: EventMessage,
		error: &E,
	) {
		let client = self.get_client();

		log::error!(
			target: &client.get_chain_name(),
			"-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(sub_target),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		sentry::capture_message(
			format!(
				"[{}]-[{}]-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
				&client.get_chain_name(),
				sub_target,
				client.address(),
				msg.metadata,
				msg.retries_remaining - 1,
				error
			)
				.as_str(),
			sentry::Level::Error,
		);
		self.retry_transaction(msg).await;
	}

	/// Handles the failed gas estimation.
	async fn handle_failed_gas_estimation<E: Error + Sync + ?Sized>(
		&self,
		sub_target: &str,
		msg: EventMessage,
		error: &E,
	) {
		let client = self.get_client();

		if self.debug_mode() {
			log::warn!(
				target: &client.get_chain_name(),
				"-[{}] ⚠️  Warning! Error encountered during gas estimation: {}, Retries left: {:?}, Error: {}",
				sub_display_format(sub_target),
				msg.metadata,
				msg.retries_remaining - 1,
				error.to_string()
			);
			sentry::capture_message(
				format!(
					"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during gas estimation: {}, Retries left: {:?}, Error: {}",
					&client.get_chain_name(),
					sub_target,
					client.address(),
					msg.metadata,
					msg.retries_remaining - 1,
					error
				)
					.as_str(),
				sentry::Level::Warning,
			);
		}
		self.retry_transaction(msg).await;
	}
}
