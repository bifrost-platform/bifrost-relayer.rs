use br_primitives::{eth::GasCoefficient, sub_display_format};
use ethers::{
	providers::{JsonRpcClient, Middleware},
	types::{TransactionRequest, U256},
};
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;

use crate::eth::{
	EthClient, EventMessage, EventMetadata, EventSender, LegacyGasMiddleware, SocketRelayMetadata,
	TxRequest, DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS,
};

use super::SocketRelayBuilder;

const SUB_LOG_TARGET: &str = "execution-filter";

/// The gas estimation result of the general message.
enum EstimationResult {
	/// The general message is executable. This contains the estimated gas amount.
	Executable(U256),
	/// The general message is non-executable. This contains the error message.
	NonExecutable(String),
}

/// The task that tries to pre-gasEstimate the general message.
/// This only handles requests that are relayed to the target client.
/// (`client` and `event_sender` are connected to the same chain)
pub struct ExecutionFilter<T> {
	/// The `EthClient` to interact with the connected blockchain.
	client: Arc<EthClient<T>>,
	/// The event senders that sends messages to the event channel.
	event_sender: Arc<EventSender>,
	/// The metadata of the given socket message.
	metadata: SocketRelayMetadata,
}

impl<T: JsonRpcClient> ExecutionFilter<T> {
	/// Instantiates a new `ExecutionFilter`.
	pub fn new(
		client: Arc<EthClient<T>>,
		event_sender: Arc<EventSender>,
		metadata: SocketRelayMetadata,
	) -> Self {
		Self { client, event_sender, metadata }
	}

	/// Tries to gas estimate the general message and on success cases,
	/// sends a transaction request to the event channel.
	pub async fn try_filter_and_send(
		&self,
		tx_request: TransactionRequest,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		if let Some(executor_adddress) = self.client.protocol_contracts.executor_address {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⛓️  Start Execution::Filter: {}",
				sub_display_format(SUB_LOG_TARGET),
				self.metadata
			);

			let general_msg = TxRequest::Legacy(
				TransactionRequest::default()
					.data(self.metadata.variants.clone().data)
					.to(self.metadata.variants.receiver)
					.from(executor_adddress)
					.gas_price(self.client.get_gas_price().await),
			);

			let estimated_gas = match self.try_gas_estimation(general_msg).await {
				EstimationResult::Executable(estimated_gas) => estimated_gas,
				EstimationResult::NonExecutable(error) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] ⛓️  Execution::Filter Result → ❗️ Failure::Error({}): {}",
						sub_display_format(SUB_LOG_TARGET),
						error,
						self.metadata
					);
					return;
				},
			};

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⛓️  Execution::Filter Result → ✅ Success::EstimatedGas({}): {}",
				sub_display_format(SUB_LOG_TARGET),
				estimated_gas,
				self.metadata
			);

			self.request_send_transaction(tx_request, give_random_delay, gas_coefficient);
		}
	}

	/// Requests a new transaction request to the event channel.
	fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		match self.event_sender.send(EventMessage::new(
			TxRequest::Legacy(tx_request),
			EventMetadata::SocketRelay(self.metadata.clone()),
			true,
			give_random_delay,
			gas_coefficient,
			self.metadata.is_bootstrap,
		)) {
			Ok(()) => {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔖 Request relay transaction to chain({:?}): {}",
					sub_display_format(SUB_LOG_TARGET),
					&self.client.get_chain_id(),
					self.metadata
				);
			},
			Err(error) => {
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to send relay transaction to chain({:?}): {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					&self.client.get_chain_id(),
					self.metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to send relay transaction to chain({:?}): {}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						&self.client.get_chain_id(),
						self.metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	/// Tries to gas estimate the given general message.
	async fn try_gas_estimation(&self, general_msg: TxRequest) -> EstimationResult {
		match self.client.provider.estimate_gas(&general_msg.to_typed(), None).await {
			Ok(estimated_gas) => {
				// this request is executable. now try to send the relay transaction.
				EstimationResult::Executable(estimated_gas)
			},
			Err(error) => {
				// this request is not executable. first, it'll be retried several times.
				// if it still remains as failure, this request will be ignored and the `SocketRollbackHandler` might handle it.
				self.handle_failed_gas_estimation(&general_msg, error.to_string()).await
			},
		}
	}

	/// Handles the failed gas estimation.
	async fn handle_failed_gas_estimation(
		&self,
		tx_request: &TxRequest,
		error: String,
	) -> EstimationResult {
		let mut retries = DEFAULT_CALL_RETRIES;
		let mut last_error = error;

		while retries > 0 {
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			if self.client.debug_mode {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during gas estimation, Retries left: {:?}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					retries - 1,
					last_error
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during gas estimation, Retries left: {:?}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						retries - 1,
						last_error
					)
					.as_str(),
					sentry::Level::Warning,
				);
			}

			match self.client.provider.estimate_gas(&tx_request.to_typed(), None).await {
				Ok(estimated_gas) => return EstimationResult::Executable(estimated_gas),
				Err(error) => {
					sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					retries -= 1;
					last_error = error.to_string();
				},
			}
		}
		EstimationResult::NonExecutable(last_error)
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> SocketRelayBuilder<T> for ExecutionFilter<T> {
	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}
}
