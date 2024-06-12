mod inbound;
mod outbound;
mod psbt_signer;

pub use inbound::*;
pub use outbound::*;
pub use psbt_signer::*;

use crate::{
	btc::{
		block::{Event, EventType},
		LOG_TARGET,
	},
	eth::EthClient,
};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	eth::{BootstrapState, GasCoefficient},
	tx::{
		TxRequest, TxRequestMessage, TxRequestMetadata, TxRequestSender, XtRequest,
		XtRequestMessage, XtRequestMetadata, XtRequestSender,
	},
	utils::sub_display_format,
};
use ethers::{prelude::TransactionRequest, providers::JsonRpcClient};
use std::sync::Arc;

use super::block::EventMessage;

#[async_trait::async_trait]
pub trait XtRequester<T: JsonRpcClient> {
	fn xt_request_sender(&self) -> Arc<XtRequestSender>;

	fn bfc_client(&self) -> Arc<EthClient<T>>;

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
pub trait TxRequester<T: JsonRpcClient> {
	fn tx_request_sender(&self) -> Arc<TxRequestSender>;

	fn bfc_client(&self) -> Arc<EthClient<T>>;

	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: TxRequestMetadata,
		sub_log_target: &str,
	) {
		match self.tx_request_sender().send(TxRequestMessage::new(
			TxRequest::Legacy(tx_request),
			metadata.clone(),
			true,
			false,
			GasCoefficient::Mid,
			false,
		)) {
			Ok(_) => log::info!(
				target: LOG_TARGET,
				"-[{}] 🔖 Request relay transaction: {}",
				sub_display_format(sub_log_target),
				metadata
			),
			Err(error) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to send relay transaction: {}, Error: {}",
					sub_display_format(sub_log_target),
					self.bfc_client().address(),
					metadata,
					error
				);
				log::error!(target: LOG_TARGET, "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", LOG_TARGET),
					sentry::Level::Error,
				);
			},
		}
	}
}

#[async_trait::async_trait]
pub trait Handler {
	async fn run(&mut self);

	async fn process_event(&self, event_tx: Event);

	fn is_target_event(&self, event_type: EventType) -> bool;
}

#[async_trait::async_trait]
pub trait BootstrapHandler {
	/// Fetch the shared bootstrap data.
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData>;

	/// Starts the bootstrap process.
	async fn bootstrap(&self);

	/// Fetch the historical events to bootstrap.
	async fn get_bootstrap_events(&self) -> (EventMessage, EventMessage);

	/// Verifies whether the bootstrap state has been synced to the given state.
	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data()
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}
