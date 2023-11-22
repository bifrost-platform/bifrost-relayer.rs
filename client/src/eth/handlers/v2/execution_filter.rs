use std::{error::Error, marker::PhantomData, sync::Arc, time::Duration};

use br_primitives::{eth::SocketEventStatus, sub_display_format};
use ethers::{
	providers::{JsonRpcClient, Middleware},
	types::{Address, Bytes, TransactionRequest, U256},
};
use tokio::time::sleep;

use crate::eth::{
	EthClient, LegacyGasMiddleware, SocketRelayMetadata, TxRequest, DEFAULT_CALL_RETRIES,
	DEFAULT_CALL_RETRY_INTERVAL_MS,
};

const SUB_LOG_TARGET: &str = "execution-filter";

pub enum FilterResult {
	/// Gas estimation has success. This contains the estimated gas amount.
	ExecutionPossible(U256),
	/// Gas estimation has failed. This contains the failure reason.
	ExecutionFailed(String),
	/// The estimated execution fee for the current request is within the maximum payable fee.
	FeePayable,
	/// The estimated execution fee for the current request exceeds the maximum payable fee.
	FeeLimitExceeds,
}

#[async_trait::async_trait]
pub trait CCCPFilter<T> {
	/// Request CCCP v2 execution filtering.
	async fn try_filter(
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
	) -> SocketEventStatus;

	/// Verify whether the request is executable on the destination chain.
	async fn filter_executable(
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
	) -> FilterResult;

	/// Verify whether the execution fee is within the maximum payable fee.
	async fn filter_max_fee(
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
		gas: U256,
	) -> FilterResult;

	/// Build a raw transaction for the execution.
	fn build_transaction_request(receiver: Address, data: Bytes) -> TransactionRequest;
}

/// The implementation used to verify and filter CCCP v2 requests.
pub struct CCCPExecutionFilter<T>(PhantomData<T>);

impl<T: JsonRpcClient> CCCPExecutionFilter<T> {
	/// Retry failed gas estimation.
	async fn handle_failed_gas_estimation<E: Error + Sync + ?Sized>(
		client: &Arc<EthClient<T>>,
		tx_request: TransactionRequest,
		error: &E,
	) -> FilterResult {
		let mut retries = DEFAULT_CALL_RETRIES;
		let mut last_error = error.to_string();

		while retries > 0 {
			br_metrics::increase_rpc_calls(&client.get_chain_name());

			if client.debug_mode {
				log::warn!(
					target: &client.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during get gas price, Retries left: {:?}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					retries - 1,
					last_error
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during get gas price, Retries left: {:?}, Error: {}",
						&client.get_chain_name(),
						SUB_LOG_TARGET,
						client.address(),
						retries - 1,
						last_error
					)
					.as_str(),
					sentry::Level::Warning,
				);
			}

			match client
				.provider
				.estimate_gas(&TxRequest::Legacy(tx_request.clone()).to_typed(), None)
				.await
			{
				Ok(estimated_gas) => return FilterResult::ExecutionPossible(estimated_gas),
				Err(error) => {
					sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					retries -= 1;
					last_error = error.to_string();
				},
			}
		}
		FilterResult::ExecutionFailed(last_error)
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> CCCPFilter<T> for CCCPExecutionFilter<T> {
	async fn try_filter(
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
	) -> SocketEventStatus {
		let failed_status = match metadata.is_inbound {
			true => SocketEventStatus::Failed,
			false => SocketEventStatus::Rejected,
		};

		match Self::filter_executable(client, metadata.clone()).await {
			FilterResult::ExecutionPossible(gas) => {
				match Self::filter_max_fee(client, metadata.clone(), gas).await {
					FilterResult::FeePayable => return metadata.status,
					FilterResult::FeeLimitExceeds => {
						log::warn!(
							target: &client.get_chain_name(),
							"-[{}] ⚠️  Relay transaction failed filter [FilterResult::FeeLimitExceeds]: {}",
							sub_display_format(SUB_LOG_TARGET),
							metadata
						);
						return failed_status;
					},
					_ => panic!("Invalid FilterResult received"),
				}
			},
			FilterResult::ExecutionFailed(reason) => {
				log::warn!(
					target: &client.get_chain_name(),
					"-[{}] ⚠️  Relay transaction failed filter [FilterResult::ExecutionFailed]: {}, {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata,
					reason
				);
				return failed_status;
			},
			_ => panic!("Invalid FilterResult received"),
		};
	}

	async fn filter_executable(
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
	) -> FilterResult {
		let mut tx_request =
			Self::build_transaction_request(metadata.receiver, metadata.variants.data);
		tx_request = tx_request.from(client.protocol_contracts.router_address);

		match client
			.provider
			.estimate_gas(&TxRequest::Legacy(tx_request.clone()).to_typed(), None)
			.await
		{
			Ok(estimated_gas) => FilterResult::ExecutionPossible(estimated_gas),
			Err(error) => Self::handle_failed_gas_estimation(client, tx_request, &error).await,
		}
	}

	async fn filter_max_fee(
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
		gas: U256,
	) -> FilterResult {
		let gas_price = client.get_gas_price().await;
		let fee = gas.saturating_mul(gas_price);
		if fee > metadata.variants.max_fee {
			return FilterResult::FeeLimitExceeds;
		}
		FilterResult::FeePayable
	}

	fn build_transaction_request(receiver: Address, data: Bytes) -> TransactionRequest {
		TransactionRequest::default().to(receiver).data(data)
	}
}
