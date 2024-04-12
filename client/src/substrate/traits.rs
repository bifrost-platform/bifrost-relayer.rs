use br_primitives::{
	substrate::CustomConfig,
	tx::{XtRequestMessage, XtRequestMetadata},
	utils::sub_display_format,
};

use ethers::providers::JsonRpcClient;
use std::{error::Error, sync::Arc, time::Duration};
use subxt::{blocks::ExtrinsicEvents, Config, OnlineClient};
use tokio::time::sleep;

use crate::eth::EthClient;

#[async_trait::async_trait]
pub trait ExtrinsicTask<T>
where
	T: JsonRpcClient,
{
	/// Get the substrate client.
	fn get_sub_client(&self) -> Arc<OnlineClient<CustomConfig>>;

	/// Get the Bifrost client.
	fn get_bfc_client(&self) -> Arc<EthClient<T>>;

	/// Sends the consumed unsigned transaction request to the Bifrost network.
	async fn try_send_unsigned_transaction(&self, msg: XtRequestMessage);

	/// Retry try_send_unsigned_transaction() for failed transaction execution.
	async fn retry_transaction(&self, mut msg: XtRequestMessage) {
		msg.build_retry_event();
		sleep(Duration::from_millis(msg.retry_interval)).await;
		self.try_send_unsigned_transaction(msg).await;
	}

	/// Handles failed transaction requests.
	async fn handle_failed_tx_request<E>(&self, sub_target: &str, msg: XtRequestMessage, error: &E)
	where
		E: Error + Sync + ?Sized,
	{
		log::error!(
			target: &self.get_bfc_client().get_chain_name(),
			"-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(sub_target),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		sentry::capture_message(
            format!(
                "[{}]-[{}]-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
                &self.get_bfc_client().get_chain_name(),
                sub_target,
                self.get_bfc_client().address(),
                msg.metadata,
                msg.retries_remaining - 1,
                error
            )
                .as_str(),
            sentry::Level::Error,
        );
		self.retry_transaction(msg).await;
	}

	/// Handles failed transaction creation.
	async fn handle_failed_tx_creation<E>(&self, sub_target: &str, msg: XtRequestMessage, error: &E)
	where
		E: Error + Sync + ?Sized,
	{
		log::error!(
			target: &self.get_bfc_client().get_chain_name(),
			"-[{}] ♻️  Unknown error while creating a tx request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(sub_target),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		sentry::capture_message(
            format!(
                "[{}]-[{}]-[{}] ♻️  Unknown error while creating a transaction request: {}, Retries left: {:?}, Error: {}",
                &self.get_bfc_client().get_chain_name(),
                sub_target,
                self.get_bfc_client().address(),
                msg.metadata,
                msg.retries_remaining - 1,
                error
            )
                .as_str(),
            sentry::Level::Error,
        );
		self.retry_transaction(msg).await;
	}

	/// Handles successful transaction requests.
	async fn handle_success_tx_request<C>(
		&self,
		sub_target: &str,
		events: ExtrinsicEvents<C>,
		metadata: XtRequestMetadata,
	) where
		C: Config,
	{
		log::info!(
			target: &self.get_bfc_client().get_chain_name(),
			"-[{}] 🎁 The requested transaction has been successfully mined in block: {}, {:?}-{:?}",
			sub_display_format(sub_target),
			metadata,
			events.extrinsic_index(),
			events.extrinsic_hash()
		);
	}
}
