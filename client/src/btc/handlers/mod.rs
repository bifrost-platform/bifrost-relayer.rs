mod inbound;
mod outbound;

use ethers::prelude::TransactionRequest;
use ethers::providers::JsonRpcClient;
pub use inbound::*;
pub use outbound::*;
use std::sync::Arc;

use crate::btc::block::{Event, EventType};
use crate::eth::EthClient;
use br_primitives::eth::{BootstrapState, GasCoefficient};
use br_primitives::sub_display_format;
use br_primitives::tx::{
	BitcoinRelayMetadata, TxRequest, TxRequestMessage, TxRequestMetadata, TxRequestSender,
};
use miniscript::bitcoin::Transaction;

pub const LOG_TARGET: &str = "Bitcoin";

#[async_trait::async_trait]
pub trait Handler<T: JsonRpcClient> {
	fn tx_request_sender(&self) -> Arc<TxRequestSender>;

	fn bfc_client(&self) -> Arc<EthClient<T>>;

	async fn run(&mut self);

	async fn process_event(&self, event_tx: Event, is_bootstrap: bool);

	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: BitcoinRelayMetadata,
		sub_log_target: &str,
	) {
		match self.tx_request_sender().send(TxRequestMessage::new(
			TxRequest::Legacy(tx_request),
			TxRequestMetadata::BitcoinSocketRelay(metadata.clone()),
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
				log::error!(
					target: LOG_TARGET,
					"-[{}] ❗️ Failed to send relay transaction: {}, Error: {}",
					sub_display_format(sub_log_target),
					metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to send relay transaction: {}, Error: {}",
						LOG_TARGET,
						sub_log_target,
						self.bfc_client().address(),
						metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	fn is_target_event(&self, event_type: EventType) -> bool;
}

#[async_trait::async_trait]
pub trait BootstrapHandler {
	async fn bootstrap(&self);

	async fn get_bootstrap_events(&self) -> Vec<Transaction>;

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool;
}
