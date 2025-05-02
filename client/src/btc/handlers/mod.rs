mod feerate_feeder;
mod inbound;
mod outbound;

pub use feerate_feeder::*;
pub use inbound::*;
pub use outbound::*;

use crate::{
	btc::{
		LOG_TARGET,
		block::{Event, EventType},
	},
	eth::EthClient,
};

use super::block::EventMessage;
use alloy::{
	network::Network,
	primitives::ChainId,
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
use tokio::time::sleep;

#[async_trait::async_trait]
pub trait XtRequester<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender>;

	fn bfc_client(&self) -> Arc<EthClient<F, P, N>>;

	async fn request_send_transaction(
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
			XtRequest::SubmitUtxos(call) => XtRequestMessage::new(call.into(), metadata.clone()),
			XtRequest::BroadcastPoll(call) => XtRequestMessage::new(call.into(), metadata.clone()),
			XtRequest::SubmitOutboundRequests(call) => {
				XtRequestMessage::new(call.into(), metadata.clone())
			},
			XtRequest::SubmitFeeRate(call) => XtRequestMessage::new(call.into(), metadata.clone()),
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
					self.bfc_client().address().await,
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
	/// Get the chain id.
	fn get_chain_id(&self) -> ChainId;

	/// Fetch the shared bootstrap data.
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData>;

	/// Starts the bootstrap process.
	async fn bootstrap(&self) -> Result<()>;

	/// Fetch the historical events to bootstrap.
	async fn get_bootstrap_events(&self) -> Result<(EventMessage, EventMessage)>;

	/// Set the bootstrap state for a chain.
	async fn set_bootstrap_state(&self, state: BootstrapState) {
		let bootstrap_shared_data = self.bootstrap_shared_data();
		let mut bootstrap_states = bootstrap_shared_data.bootstrap_states.write().await;
		*bootstrap_states.get_mut(&self.get_chain_id()).unwrap() = state;
	}

	/// Verifies whether the given chain is before the given bootstrap state.
	async fn is_before_bootstrap_state(&self, state: BootstrapState) -> bool {
		*self
			.bootstrap_shared_data()
			.bootstrap_states
			.read()
			.await
			.get(&self.get_chain_id())
			.unwrap() < state
	}

	/// Waits for the bootstrap state to be synced to the normal start state.
	async fn wait_for_bootstrap_state(&self, state: BootstrapState) -> Result<()> {
		loop {
			let current_state = {
				let shared_data = self.bootstrap_shared_data();
				let bootstrap_states = shared_data.bootstrap_states.read().await;
				*bootstrap_states.get(&self.get_chain_id()).unwrap()
			};

			if current_state == state {
				break;
			}
			sleep(Duration::from_millis(100)).await;
		}
		Ok(())
	}

	/// Waits for all chains to be bootstrapped.
	async fn wait_for_all_chains_bootstrapped(&self) -> Result<()> {
		loop {
			let all_bootstrapped = {
				let shared_data = self.bootstrap_shared_data();
				let bootstrap_states = shared_data.bootstrap_states.read().await;
				bootstrap_states.values().all(|state| *state == BootstrapState::NormalStart)
			};

			if all_bootstrapped {
				break;
			}
			sleep(Duration::from_millis(100)).await;
		}
		Ok(())
	}

	/// Waits for the bootstrap state to be synced to the normal start state.
	async fn wait_for_normal_state(&self) -> Result<()> {
		while !self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
			sleep(Duration::from_millis(100)).await;
		}
		Ok(())
	}
}
