use crate::traits::PeriodicWorker;
use br_client::eth::EthClient;
use br_primitives::{
	constants::{
		errors::{INVALID_BIFROST_NATIVENESS, INVALID_PERIODIC_SCHEDULE},
		schedule::HEARTBEAT_SCHEDULE,
	},
	eth::GasCoefficient,
	substrate::CustomConfig,
	tx::{HeartbeatMetadata, TxRequest, TxRequestMessage, TxRequestMetadata, TxRequestSender},
	utils::sub_display_format,
};
use cron::Schedule;
use ethers::{providers::JsonRpcClient, types::TransactionRequest};
use std::{str::FromStr, sync::Arc};
use subxt::tx::Signer;

const SUB_LOG_TARGET: &str = "heartbeat-sender";

/// The essential task that sending heartbeat transaction.
pub struct HeartbeatSender<T, S> {
	/// The time schedule that represents when to check heartbeat pulsed.
	schedule: Schedule,
	/// The sender that sends messages to the tx request channel.
	tx_request_sender: Arc<TxRequestSender>,
	/// The `EthClient` to interact with the bifrost network.
	client: Arc<EthClient<T, S>>,
}

#[async_trait::async_trait]
impl<T, S> PeriodicWorker for HeartbeatSender<T, S>
where
	T: JsonRpcClient + 'static,
	S: Signer<CustomConfig> + 'static + Send + Sync,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			let address = self.client.address();

			let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
			let is_selected = self
				.client
				.contract_call(
					relayer_manager.is_selected_relayer(address, false),
					"relayer_manager.is_selected_relayer",
				)
				.await;
			let is_heartbeat_pulsed = self
				.client
				.contract_call(
					relayer_manager.is_heartbeat_pulsed(address),
					"relayer_manager.is_heartbeat_pulsed",
				)
				.await;

			if is_selected && !is_heartbeat_pulsed {
				let round_info = self
					.client
					.contract_call(
						self.client.protocol_contracts.authority.round_info(),
						"authority.round_info",
					)
					.await;
				self.request_send_transaction(
					self.build_transaction(),
					HeartbeatMetadata::new(
						round_info.current_round_index,
						round_info.current_session_index,
					),
				)
				.await;
			}
			self.wait_until_next_time().await;
		}
	}
}

impl<T, S> HeartbeatSender<T, S>
where
	T: JsonRpcClient + 'static,
	S: Signer<CustomConfig> + 'static + Send + Sync,
{
	/// Instantiates a new `HeartbeatSender` instance.
	pub fn new(
		tx_request_senders: Vec<Arc<TxRequestSender>>,
		system_clients: Vec<Arc<EthClient<T, S>>>,
	) -> Self {
		Self {
			schedule: Schedule::from_str(HEARTBEAT_SCHEDULE).expect(INVALID_PERIODIC_SCHEDULE),
			tx_request_sender: tx_request_senders
				.iter()
				.find(|sender| sender.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
			client: system_clients
				.iter()
				.find(|client| client.metadata.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
		}
	}

	/// Build `heartbeat` transaction.
	fn build_transaction(&self) -> TransactionRequest {
		let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
		TransactionRequest::default()
			.to(relayer_manager.address())
			.data(relayer_manager.heartbeat().calldata().unwrap())
	}

	/// Request send transaction to the target tx request channel.
	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: HeartbeatMetadata,
	) {
		match self.tx_request_sender.send(TxRequestMessage::new(
			TxRequest::Legacy(tx_request),
			TxRequestMetadata::Heartbeat(metadata.clone()),
			false,
			false,
			GasCoefficient::Low,
			false,
		)) {
			Ok(()) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 💓 Request Heartbeat transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => log::error!(
				target: &self.client.get_chain_name(),
				"-[{}] ❗️ Failed to request Heartbeat transaction: {}, Error: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata,
				error.to_string()
			),
		}
	}
}
