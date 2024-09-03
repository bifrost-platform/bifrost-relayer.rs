use br_primitives::{
	substrate::CustomConfig,
	tx::{XtRequestMessage, XtRequestMetadata},
	utils::sub_display_format,
};

use ethers::providers::JsonRpcClient;
use std::{error::Error, sync::Arc, time::Duration};
use subxt::tx::Signer;
use subxt::{blocks::ExtrinsicEvents, Config, OnlineClient};
use tokio::time::sleep;

use crate::eth::EthClient;

#[async_trait::async_trait]
pub trait ExtrinsicTask<T, S>
where
	T: JsonRpcClient + 'static,
	S: Signer<CustomConfig>,
{
	/// Get the substrate client.
	fn get_sub_client(&self) -> Arc<OnlineClient<CustomConfig>>;

	/// Get the Bifrost client.
	fn get_bfc_client(&self) -> Arc<EthClient<T, S>>;

	/// Sends the consumed extrinsic request to the Bifrost network.
	async fn try_send_extrinsic(&self, msg: XtRequestMessage);

	/// Retry `try_send_extrinsic()` for failed extrinsic execution.
	async fn retry_extrinsic(&self, mut msg: XtRequestMessage) {
		msg.build_retry_event();
		sleep(Duration::from_millis(msg.retry_interval)).await;
		self.try_send_extrinsic(msg).await;
	}

	/// Handles failed extrinsic requests.
	async fn handle_failed_xt_request<E>(&self, sub_target: &str, msg: XtRequestMessage, error: &E)
	where
		E: Error + Sync + ?Sized,
	{
		let log_msg = format!(
			"-[{}]-[{}] ♻️  Unknown error while requesting a extrinsic request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(sub_target),
			self.get_bfc_client().address(),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		log::error!(target: &self.get_bfc_client().get_chain_name(), "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", &self.get_bfc_client().get_chain_name()),
			sentry::Level::Error,
		);

		self.retry_extrinsic(msg).await;
	}

	/// Handles successful extrinsic requests.
	async fn handle_success_xt_request<C>(
		&self,
		sub_target: &str,
		events: ExtrinsicEvents<C>,
		metadata: XtRequestMetadata,
	) where
		C: Config,
	{
		log::info!(
			target: &self.get_bfc_client().get_chain_name(),
			"-[{}] 🎁 The requested extrinsic has been successfully mined in block: {}, {:?}-{:?}",
			sub_display_format(sub_target),
			metadata,
			events.extrinsic_index(),
			events.extrinsic_hash()
		);
	}
}
