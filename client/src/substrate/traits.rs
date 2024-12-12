use alloy::{
	network::AnyNetwork,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	transports::Transport,
};
use br_primitives::{
	substrate::CustomConfig,
	tx::{XtRequestMessage, XtRequestMetadata},
	utils::sub_display_format,
};

use std::{error::Error, sync::Arc, time::Duration};
use subxt::{blocks::ExtrinsicEvents, Config, OnlineClient};
use tokio::time::sleep;

use crate::eth::EthClient;

#[async_trait::async_trait]
pub trait ExtrinsicTask<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Get the substrate client.
	fn get_sub_client(&self) -> Arc<OnlineClient<CustomConfig>>;

	/// Get the Bifrost client.
	fn get_bfc_client(&self) -> Arc<EthClient<F, P, T>>;

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
		let error_str = error.to_string();

		// If the transaction is a SubmitSignedPsbt, and the error is RequestDNE, then we don't need to retry
		if let XtRequestMetadata::SubmitSignedPsbt(_) = msg.metadata {
			if error_str.contains("BtcSocketQueue::RequestDNE") {
				log::info!(
					target: &self.get_bfc_client().get_chain_name(),
					"M-of-N signature has been collected, so not retrying"
				);
				return;
			}
		}

		let log_msg = format!(
			"-[{}]-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(sub_target),
			self.get_bfc_client().address(),
			msg.metadata,
			msg.retries_remaining - 1,
			error_str,
		);
		log::error!(target: &self.get_bfc_client().get_chain_name(), "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", &self.get_bfc_client().get_chain_name()),
			sentry::Level::Error,
		);

		self.retry_transaction(msg).await;
	}

	/// Handles failed transaction creation.
	async fn handle_failed_tx_creation<E>(&self, sub_target: &str, msg: XtRequestMessage, error: &E)
	where
		E: Error + Sync + ?Sized,
	{
		let log_msg = format!(
			"-[{}]-[{}] ♻️  Unknown error while creating a tx request: {}, Retries left: {:?}, Error: {}",
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
