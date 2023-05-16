use cccp_client::eth::{EthClient, EventMessage, EventMetadata, EventSender, HeartbeatMetadata};
use cccp_primitives::{
	errors::{INVALID_BIFROST_NATIVENESS, INVALID_CONTRACT_ADDRESS, INVALID_PERIODIC_SCHEDULE},
	relayer_manager::RelayerManagerContract,
	sub_display_format, PeriodicWorker,
};
use cron::Schedule;
use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{TransactionRequest, H160},
};
use std::{str::FromStr, sync::Arc};
use tokio::time::sleep;

const SUB_LOG_TARGET: &str = "heartbeat";

/// The essential task that sending heartbeat transaction.
pub struct HeartbeatSender<T> {
	/// The time schedule that represents when to check heartbeat pulsed.
	pub schedule: Schedule,
	/// The target RelayerManger contract instance.
	pub relayer_manager: RelayerManagerContract<Provider<T>>,
	/// The event sender that sends messages to the event channel.
	pub event_sender: Arc<EventSender>,
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for HeartbeatSender<T> {
	async fn run(&mut self) {
		loop {
			let address = self.client.address();

			let is_selected = self
				.client
				.contract_call(
					self.relayer_manager.is_selected_relayer(address, true),
					"relayer_manager.is_selected_relayer",
				)
				.await;
			let is_heartbeat_pulsed = self
				.client
				.contract_call(
					self.relayer_manager.is_heartbeat_pulsed(address),
					"relayer_manager.is_heartbeat_pulsed",
				)
				.await;

			if is_selected && !is_heartbeat_pulsed {
				let round_info = self
					.client
					.contract_call(self.client.authority.round_info(), "authority.round_info")
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

	async fn wait_until_next_time(&self) {
		// calculate sleep duration for next schedule
		let sleep_duration =
			self.schedule.upcoming(chrono::Utc).next().unwrap() - chrono::Utc::now();

		sleep(sleep_duration.to_std().unwrap()).await;
	}
}

impl<T: JsonRpcClient> HeartbeatSender<T> {
	/// Instantiates a new `HeartbeatSender` instance.
	pub fn new(
		schedule: String,
		client: Arc<EthClient<T>>,
		event_senders: Vec<Arc<EventSender>>,
		relayer_manager_address: String,
	) -> Self {
		Self {
			schedule: Schedule::from_str(&schedule).expect(INVALID_PERIODIC_SCHEDULE),
			relayer_manager: RelayerManagerContract::new(
				H160::from_str(&relayer_manager_address).expect(INVALID_CONTRACT_ADDRESS),
				client.get_provider(),
			),
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
		TransactionRequest::default()
			.to(self.relayer_manager.address())
			.data(self.relayer_manager.heartbeat().calldata().unwrap())
	}

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: HeartbeatMetadata,
	) {
		match self.event_sender.send(EventMessage::new(
			tx_request,
			EventMetadata::Heartbeat(metadata.clone()),
			false,
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
