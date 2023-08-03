use br_client::eth::{
	EthClient, EventMessage, EventMetadata, EventSender, HeartbeatMetadata, TxRequest,
};
use br_primitives::{
	errors::{INVALID_BIFROST_NATIVENESS, INVALID_PERIODIC_SCHEDULE},
	eth::GasCoefficient,
	sub_display_format, PeriodicWorker,
};
use cron::Schedule;
use ethers::{providers::JsonRpcClient, types::TransactionRequest};
use std::{str::FromStr, sync::Arc};

const SUB_LOG_TARGET: &str = "heartbeat";

/// The essential task that sending heartbeat transaction.
pub struct HeartbeatSender<T> {
	/// The time schedule that represents when to check heartbeat pulsed.
	pub schedule: Schedule,
	/// The event sender that sends messages to the event channel.
	pub event_sender: Arc<EventSender>,
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<T>>,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for HeartbeatSender<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			let address = self.client.address();

			let relayer_manager = self.client.contracts.relayer_manager.as_ref().unwrap();
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
						self.client.contracts.authority.round_info(),
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

impl<T: JsonRpcClient> HeartbeatSender<T> {
	/// Instantiates a new `HeartbeatSender` instance.
	pub fn new(
		schedule: String,
		client: Arc<EthClient<T>>,
		event_senders: Vec<Arc<EventSender>>,
	) -> Self {
		Self {
			schedule: Schedule::from_str(&schedule).expect(INVALID_PERIODIC_SCHEDULE),
			event_sender: event_senders
				.iter()
				.find(|channel| channel.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
			client,
		}
	}

	/// Build `heartbeat` transaction.
	fn build_transaction(&self) -> TransactionRequest {
		let relayer_manager = self.client.contracts.relayer_manager.as_ref().unwrap();
		TransactionRequest::default()
			.to(relayer_manager.address())
			.data(relayer_manager.heartbeat().calldata().unwrap())
	}

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: HeartbeatMetadata,
	) {
		match self.event_sender.send(EventMessage::new(
			TxRequest::Legacy(tx_request),
			EventMetadata::Heartbeat(metadata.clone()),
			false,
			false,
			GasCoefficient::Low,
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
