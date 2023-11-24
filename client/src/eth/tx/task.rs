use std::{cmp::max, error::Error, time::Duration};

use br_primitives::{eth::ETHEREUM_BLOCK_TIME, sub_display_format, INSUFFICIENT_FUNDS};
use ethers::{
	providers::{JsonRpcClient, Middleware},
	types::{TransactionReceipt, U256},
};
use sc_service::Arc;
use tokio::time::sleep;

use crate::eth::{
	Eip1559GasMiddleware, EthClient, EventMessage, EventMetadata, LegacyGasMiddleware, TxRequest,
	DEFAULT_TX_RETRIES, MAX_FEE_COEFFICIENT,
};

use super::{generate_delay, Eip1559Middleware, LegacyMiddleware};

const SUB_LOG_TARGET: &str = "tx-task";

/// The transaction task for Eip1559 transactions.
pub struct Eip1559TransactionTask<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: Eip1559Middleware<T>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The minimum priority fee required.
	min_priority_fee: U256,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	duplicate_confirm_delay: Option<u64>,
}

impl<T: JsonRpcClient> Eip1559TransactionTask<T> {
	/// Build an Eip1559 transaction task.
	pub fn new(
		client: Arc<EthClient<T>>,
		middleware: Eip1559Middleware<T>,
		is_txpool_enabled: bool,
		min_priority_fee: U256,
		duplicate_confirm_delay: Option<u64>,
	) -> Self {
		Self { client, middleware, is_txpool_enabled, min_priority_fee, duplicate_confirm_delay }
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> AsyncTransactionTask<T> for Eip1559TransactionTask<T> {
	fn is_txpool_enabled(&self) -> bool {
		self.is_txpool_enabled
	}

	fn debug_mode(&self) -> bool {
		self.client.debug_mode
	}

	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}

	fn duplicate_confirm_delay(&self) -> Duration {
		Duration::from_millis(self.duplicate_confirm_delay.unwrap_or(ETHEREUM_BLOCK_TIME * 1000))
	}

	async fn try_send_transaction(&self, mut msg: EventMessage) {
		if msg.retries_remaining == 0 {
			return;
		}

		// sets a random delay on external chain transactions on first try
		if msg.give_random_delay && msg.retries_remaining == DEFAULT_TX_RETRIES {
			sleep(Duration::from_millis(generate_delay())).await;
		}

		// set transaction `from` field
		msg.tx_request = msg.tx_request.from(self.client.address());

		// estimate the gas amount to be used
		let estimated_gas =
			match self.middleware.estimate_gas(&msg.tx_request.to_typed(), None).await {
				Ok(estimated_gas) => {
					br_metrics::increase_rpc_calls(&self.client.get_chain_name());
					U256::from(
						(estimated_gas.as_u64() as f64 * msg.gas_coefficient.into_f64()).ceil()
							as u64,
					)
				},
				Err(error) => {
					return self.handle_failed_gas_estimation(SUB_LOG_TARGET, msg, &error).await
				},
			};
		msg.tx_request = msg.tx_request.gas(estimated_gas);

		// check the txpool for transaction duplication prevention
		if !(self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await) {
			// no duplication found
			let fees = self.client.get_estimated_eip1559_fees().await;
			let priority_fee = max(fees.1, self.min_priority_fee);
			let max_fee_per_gas =
				fees.0.saturating_add(priority_fee).saturating_mul(MAX_FEE_COEFFICIENT.into());

			if !self.is_sufficient_funds(max_fee_per_gas, estimated_gas).await {
				panic!(
					"[{}]-[{}]-[{}] {}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					INSUFFICIENT_FUNDS,
				);
			}

			let result = self
				.middleware
				.send_transaction(
					msg.tx_request
						.to_eip1559()
						.max_fee_per_gas(max_fee_per_gas)
						.max_priority_fee_per_gas(priority_fee),
					None,
				)
				.await;
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			match result {
				Ok(pending_tx) => match pending_tx.await {
					Ok(receipt) => {
						if let Some(receipt) = receipt {
							self.handle_success_tx_receipt(
								SUB_LOG_TARGET,
								Some(receipt),
								msg.metadata,
							);
						} else {
							self.handle_failed_tx_receipt(SUB_LOG_TARGET, msg).await;
						}
					},
					Err(error) => {
						self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await;
					},
				},
				Err(error) => {
					self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await;
				},
			}
		}
	}
}

/// The transaction task for Legacy transactions.
pub struct LegacyTransactionTask<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: LegacyMiddleware<T>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The flag whether if the gas price will be initially escalated. The `escalate_percentage`
	/// will be used on escalation. This will only have effect on legacy transactions. (default:
	/// false)
	is_initially_escalated: bool,
	/// The coefficient used on transaction gas price escalation (default: 1.15)
	gas_price_coefficient: f64,
	/// The minimum value use for gas_price. (default: 0)
	min_gas_price: U256,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	duplicate_confirm_delay: Option<u64>,
}

impl<T: JsonRpcClient> LegacyTransactionTask<T> {
	/// Build an Legacy transaction task.
	pub fn new(
		client: Arc<EthClient<T>>,
		middleware: LegacyMiddleware<T>,
		is_txpool_enabled: bool,
		is_initially_escalated: bool,
		gas_price_coefficient: f64,
		min_gas_price: U256,
		duplicate_confirm_delay: Option<u64>,
	) -> Self {
		Self {
			client,
			middleware,
			is_txpool_enabled,
			is_initially_escalated,
			gas_price_coefficient,
			min_gas_price,
			duplicate_confirm_delay,
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> AsyncTransactionTask<T> for LegacyTransactionTask<T> {
	fn is_txpool_enabled(&self) -> bool {
		self.is_txpool_enabled
	}

	fn debug_mode(&self) -> bool {
		self.client.debug_mode
	}

	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}

	fn duplicate_confirm_delay(&self) -> Duration {
		Duration::from_millis(self.duplicate_confirm_delay.unwrap_or(ETHEREUM_BLOCK_TIME * 1000))
	}

	async fn try_send_transaction(&self, mut msg: EventMessage) {
		if msg.retries_remaining == 0 {
			return;
		}

		// sets a random delay on external chain transactions on first try
		if msg.give_random_delay && msg.retries_remaining == DEFAULT_TX_RETRIES {
			sleep(Duration::from_millis(generate_delay())).await;
		}

		// set transaction `from` field
		msg.tx_request = msg.tx_request.from(self.client.address());

		// estimate the gas amount to be used
		let estimated_gas =
			match self.middleware.estimate_gas(&msg.tx_request.to_typed(), None).await {
				Ok(estimated_gas) => {
					br_metrics::increase_rpc_calls(&self.client.get_chain_name());
					U256::from(
						(estimated_gas.as_u64() as f64 * msg.gas_coefficient.into_f64()).ceil()
							as u64,
					)
				},
				Err(error) => {
					return self.handle_failed_gas_estimation(SUB_LOG_TARGET, msg, &error).await
				},
			};
		msg.tx_request = msg.tx_request.gas(estimated_gas);

		// check the txpool for transaction duplication prevention
		if !(self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await) {
			let mut tx = msg.tx_request.to_legacy();
			let gas_price = self.client.get_gas_price().await;
			if !self.is_sufficient_funds(gas_price, estimated_gas).await {
				panic!(
					"[{}]-[{}]-[{}] {}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					INSUFFICIENT_FUNDS,
				);
			}

			if self.is_initially_escalated {
				tx = tx.gas_price(
					self.client
						.get_gas_price_for_escalation(
							gas_price,
							self.gas_price_coefficient,
							self.min_gas_price,
						)
						.await,
				);
			}
			let result = self.middleware.send_transaction(tx, None).await;
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			match result {
				Ok(pending_tx) => match pending_tx.await {
					Ok(receipt) => {
						self.handle_success_tx_receipt(SUB_LOG_TARGET, receipt, msg.metadata)
					},
					Err(error) => self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await,
				},
				Err(error) => self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await,
			}
		}
	}
}

#[async_trait::async_trait]
pub trait AsyncTransactionTask<T>
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
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						true
					} else {
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						false
					};
				}
			}
		}
		false
	}

	/// Retry send_transaction() for failed transaction execution.
	async fn retry_transaction(&self, mut msg: EventMessage) {
		msg.build_retry_event();
		sleep(Duration::from_millis(msg.retry_interval)).await;
		self.try_send_transaction(msg).await;
	}

	/// Sends the consumed transaction request to the connected chain. The transaction send will
	/// be retry if the transaction fails to be mined in a block.
	async fn try_send_transaction(&self, msg: EventMessage);

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
