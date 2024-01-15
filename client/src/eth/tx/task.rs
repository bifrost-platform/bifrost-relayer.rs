use std::{error::Error, time::Duration};

use br_primitives::sub_display_format;
use ethers::types::TxHash;
use ethers::{
	providers::JsonRpcClient,
	types::{TransactionReceipt, U256},
};
use sc_service::Arc;
use tokio::time::sleep;

use crate::eth::{EthClient, EventMessage, EventMetadata, TxRequest};

#[async_trait::async_trait]
pub trait TransactionTask<T>
where
	T: JsonRpcClient,
{
	/// The flag whether the client has enabled txpool namespace.
	fn is_txpool_enabled(&self) -> bool;

	/// Get the `EthClient`.
	fn get_client(&self) -> Arc<EthClient<T>>;

	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	fn duplicate_confirm_delay(&self) -> Duration;

	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged such as
	/// gas estimation failures.
	fn debug_mode(&self) -> bool;

	/// Verifies whether the relayer has sufficient funds to pay for the transaction.
	async fn is_sufficient_funds(&self, gas_price: U256, gas: U256) -> bool {
		let fee = gas_price.saturating_mul(gas);
		let client = self.get_client();
		let balance = client.get_balance(client.address()).await;
		br_metrics::increase_rpc_calls(&client.get_chain_name());
		if balance < fee {
			return false;
		}
		true
	}

	/// Function that query mempool for check if the relay event that this relayer is about to send
	/// has already been processed by another relayer.
	async fn is_duplicate_relay(&self, tx_request: &TxRequest, check_mempool: bool) -> bool {
		let client = self.get_client();

		// does not check the txpool if the following condition satisfies
		// 1. the txpool namespace is disabled for the client
		// 2. the txpool check flag is false
		// 3. the client is Bifrost (native)
		if !self.is_txpool_enabled() || !check_mempool || client.metadata.is_native {
			return false;
		}

		let (data, to, from) = (
			tx_request.get_data(),
			tx_request.get_to().as_address().unwrap(),
			tx_request.get_from(),
		);

		let mempool = client.get_txpool_content().await;
		br_metrics::increase_rpc_calls(&client.get_chain_name());

		for (_address, tx_map) in mempool.pending.iter().chain(mempool.queued.iter()) {
			for (_nonce, mempool_tx) in tx_map.iter() {
				if mempool_tx.to.unwrap_or_default() == *to && mempool_tx.input == *data {
					// Trying gas escalating is not duplicate action
					if mempool_tx.from == *from {
						return false;
					}

					sleep(self.duplicate_confirm_delay()).await;
					return if client.get_transaction_receipt(mempool_tx.hash).await.is_some() {
						// if others relay processed
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						true
					} else {
						// if others relay stalled in mempool
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						false
					};
				}
			}
		}
		false
	}

	/// Retry send_transaction() for failed transaction execution.
	async fn retry_transaction(&self, mut msg: EventMessage, escalation: bool) {
		if !escalation {
			msg.build_retry_event();
			sleep(Duration::from_millis(msg.retry_interval)).await;
		}
		self.try_send_transaction(msg).await;
	}

	/// Sends the consumed transaction request to the connected chain. The transaction send will
	/// be retry if the transaction fails to be mined in a block.
	async fn try_send_transaction(&self, msg: EventMessage);

	/// Handles the successful transaction receipt.
	fn handle_success_tx_receipt(
		&self,
		sub_target: &str,
		receipt: TransactionReceipt,
		metadata: EventMetadata,
	) {
		let client = self.get_client();

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
	}

	/// Handles the stalled transaction.
	async fn handle_stalled_tx(&self, sub_target: &str, msg: EventMessage, pending: TxHash) {
		let client = self.get_client();
		log::warn!(
			target: &client.get_chain_name(),
			"-[{}] ♻️ The pending transaction has been stalled over 3 blocks. Try gas-escalation: {}-{}",
			sub_display_format(sub_target),
			msg.metadata,
			pending,
		);
		sentry::capture_message(
			format!(
				"[{}]-[{}]-[{}] ♻️  Unknown error while requesting a transaction request: {}-{}",
				&client.get_chain_name(),
				sub_target,
				client.address(),
				msg.metadata,
				pending
			)
			.as_str(),
			sentry::Level::Warning,
		);
		self.retry_transaction(msg, true).await;
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
		self.retry_transaction(msg, false).await;
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
		self.retry_transaction(msg, false).await;
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
		self.retry_transaction(msg, false).await;
	}
}
