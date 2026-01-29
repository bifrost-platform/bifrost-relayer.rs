use alloy::{
	network::Network,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use br_primitives::{
	substrate::CustomConfig,
	tx::{XtRequestMessage, XtRequestMetadata},
	utils::sub_display_format,
};

use std::{error::Error, sync::Arc, time::Duration};
use subxt::{Config, OnlineClient, blocks::ExtrinsicEvents};
use tokio::time::sleep;

use crate::eth::EthClient;

#[async_trait::async_trait]
pub trait ExtrinsicTask<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Get the substrate client.
	fn get_sub_client(&self) -> Arc<OnlineClient<CustomConfig>>;

	/// Get the Bifrost client.
	fn get_bfc_client(&self) -> Arc<EthClient<F, P, N>>;

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

		// Some pallet errors are not retryable
		match msg.metadata {
			XtRequestMetadata::SubmitSignedPsbt(_) => {
				if error_str.contains("BtcSocketQueue::RequestDNE") {
					log::info!(
						target: &self.get_bfc_client().get_chain_name(),
						"M-of-N signature has been collected, so not retrying"
					);
					return;
				}
			},
			XtRequestMetadata::SubmitExecutedRequest(_) => {
				if error_str.contains("BtcSocketQueue::RequestDNE") {
					log::info!(
						target: &self.get_bfc_client().get_chain_name(),
						"Executed notification has been collected, no need to retry"
					);
					return;
				}
			},
			XtRequestMetadata::BroadcastPoll(_) => {
				if error_str.contains("Blaze::AlreadySpent") {
					log::info!(
						target: &self.get_bfc_client().get_chain_name(),
						"UTXO has been spent, no need to retry"
					);
					return;
				}
			},
			XtRequestMetadata::FinalizePoll(_) => {
				if error_str.contains("CCCPRelayQueue::TransferAlreadyFinalized") {
					log::info!(
						target: &self.get_bfc_client().get_chain_name(),
						"Transfer already finalized, no need to retry"
					);
					return;
				}
			},
			_ => {},
		};

		br_primitives::log_and_capture!(
			error,
			&self.get_bfc_client().get_chain_name(),
			sub_target,
			self.get_bfc_client().address().await,
			"♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			msg.metadata,
			msg.retries_remaining - 1,
			error_str
		);

		self.retry_transaction(msg).await;
	}

	/// Handles failed transaction creation.
	async fn handle_failed_tx_creation<E>(&self, sub_target: &str, msg: XtRequestMessage, error: &E)
	where
		E: Error + Sync + ?Sized,
	{
		br_primitives::log_and_capture!(
			error,
			&self.get_bfc_client().get_chain_name(),
			sub_target,
			self.get_bfc_client().address().await,
			"♻️  Unknown error while creating a tx request: {}, Retries left: {:?}, Error: {}",
			msg.metadata,
			msg.retries_remaining - 1,
			error
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
