mod inbound;
mod outbound;

pub use inbound::*;
pub use outbound::*;
use tokio::time::sleep;

use crate::{
	btc::{
		LOG_TARGET,
		block::{Event, EventType},
	},
	eth::EthClient,
};

use alloy::{
	network::AnyNetwork,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	eth::BootstrapState,
	tx::{XtRequest, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};
use eyre::Result;
use std::{sync::Arc, time::Duration};

use super::block::EventMessage;

#[async_trait::async_trait]
pub trait XtRequester<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender>;

	fn bfc_client(&self) -> Arc<EthClient<F, P>>;

	fn request_send_transaction(
		&self,
		xt_request: XtRequest,
		metadata: XtRequestMetadata,
		sub_log_target: &str,
	) {
		let msg = match xt_request {
			XtRequest::SubmitSignedPsbt(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
			XtRequest::SubmitVaultKey(call) => XtRequestMessage::new(call.into(), metadata.clone()),
			XtRequest::SubmitUnsignedPsbt(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
			XtRequest::SubmitExecutedRequest(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
			XtRequest::SubmitSystemVaultKey(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
			XtRequest::SubmitRollbackPoll(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
			XtRequest::VaultKeyPresubmission(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
			XtRequest::ApproveSetRefunds(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
		};
		match self.xt_request_sender().send(msg) {
			Ok(_) => log::info!(
				target: &self.bfc_client().get_chain_name(),
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(sub_log_target),
				metadata
			),
			Err(error) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
					sub_display_format(sub_log_target),
					self.bfc_client().address(),
					metadata,
					error
				);
				log::error!(target: &self.bfc_client().get_chain_name(), "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", &self.bfc_client().get_chain_name()),
					sentry::Level::Error,
				);
			},
		}
	}
}

#[async_trait::async_trait]
pub trait Handler {
	async fn run(&mut self) -> Result<()>;

	async fn process_event(&self, event_tx: Event) -> Result<()>;

	fn is_target_event(&self, event_type: EventType) -> bool;
}

#[async_trait::async_trait]
pub trait BootstrapHandler {
	/// Fetch the shared bootstrap data.
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData>;

	/// Starts the bootstrap process.
	async fn bootstrap(&self) -> Result<()>;

	/// Fetch the historical events to bootstrap.
	async fn get_bootstrap_events(&self) -> Result<(EventMessage, EventMessage)>;

	/// Waits for the bootstrap state to be synced to the normal start state.
	async fn wait_for_bootstrap_state(&self, state: BootstrapState) -> Result<()> {
		while *self.bootstrap_shared_data().bootstrap_state.read().await < state {
			sleep(Duration::from_millis(100)).await;
		}
		Ok(())
	}
}
