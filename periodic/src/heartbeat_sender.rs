use cccp_client::eth::{
	EthClient, EventMessage, EventMetadata, EventSender, HeartbeatMetadata, DEFAULT_RETRIES,
};
use cccp_primitives::{
	authority_bifrost::AuthorityBifrost, cli::HeartbeatSenderConfig,
	relayer_bifrost::RelayerManagerBifrost, sub_display_format, PeriodicWorker,
};
use cron::Schedule;
use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{TransactionRequest, H160, U256},
};
use std::{str::FromStr, sync::Arc};
use tokio::time::sleep;

const SUB_LOG_TARGET: &str = "heartbeat";

/// The essential task that sending heartbeat transaction.
pub struct HeartbeatSender<T> {
	/// The time schedule that represents when to check heartbeat pulsed.
	pub schedule: Schedule,
	/// The target RelayerManger contract instance.
	pub relayer_manager: RelayerManagerBifrost<Provider<T>>,
	/// The target Authority contract instance.
	pub authority: AuthorityBifrost<Provider<T>>,
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

			if self.relayer_manager.is_selected_relayer(address, true).call().await.unwrap() {
				if !(self.relayer_manager.is_heartbeat_pulsed(address).call().await.unwrap()) {
					let round_info: (U256, U256, U256, U256, U256, U256, U256, U256) =
						self.authority.round_info().call().await.unwrap();
					self.request_send_transaction(
						self.build_transaction(),
						HeartbeatMetadata::new(round_info.0, round_info.2),
					)
					.await;
				}
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
		config: HeartbeatSenderConfig,
		client: Arc<EthClient<T>>,
		event_senders: Vec<Arc<EventSender>>,
	) -> Self {
		Self {
			schedule: Schedule::from_str(&config.schedule)
				.expect("Failed to parse the heartbeat schedule"),
			relayer_manager: RelayerManagerBifrost::new(
				H160::from_str(&config.relayer_manager_address)
					.expect("Failed to parse the relayer manager address"),
				client.get_provider(),
			),
			authority: AuthorityBifrost::new(
				H160::from_str(&config.authority_address)
					.expect("Failed to parse the authority address"),
				client.get_provider(),
			),
			event_sender: event_senders
				.iter()
				.find(|channel| channel.is_native)
				.expect("Failed to find a event sender for bifrost network")
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
		match self.event_sender.sender.send(EventMessage::new(
			DEFAULT_RETRIES,
			tx_request,
			EventMetadata::Heartbeat(metadata.clone()),
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
