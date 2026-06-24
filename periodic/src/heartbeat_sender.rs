use crate::traits::PeriodicWorker;
use alloy::{
	network::Network,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use br_client::eth::{EthClient, send_transaction};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::HEARTBEAT_SCHEDULE},
	tx::HeartbeatMetadata,
};
use cron::Schedule;
use eyre::Result;
use sc_service::SpawnTaskHandle;
use std::{str::FromStr, sync::Arc};

const SUB_LOG_TARGET: &str = "heartbeat-sender";

/// The essential task that sending heartbeat transaction.
pub struct HeartbeatSender<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The time schedule that represents when to check heartbeat pulsed.
	schedule: Schedule,
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<F, P, N>>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

#[async_trait::async_trait]
impl<F, P, N: Network> PeriodicWorker for HeartbeatSender<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		loop {
			let address = self.client.address().await;

			let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
			let is_selected = relayer_manager.is_selected_relayer(address, false).call().await?;
			let is_heartbeat_pulsed = relayer_manager.is_heartbeat_pulsed(address).call().await?;

			if is_selected && !is_heartbeat_pulsed {
				let round_info =
					self.client.protocol_contracts.authority.round_info().call().await?;
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

impl<F, P, N: Network> HeartbeatSender<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// Instantiates a new `HeartbeatSender` instance.
	pub fn new(client: Arc<EthClient<F, P, N>>, handle: SpawnTaskHandle, debug_mode: bool) -> Self {
		Self {
			schedule: Schedule::from_str(HEARTBEAT_SCHEDULE).expect(INVALID_PERIODIC_SCHEDULE),
			client,
			handle,
			debug_mode,
		}
	}

	/// Build `heartbeat` transaction.
	fn build_transaction(&self) -> N::TransactionRequest {
		self.client
			.protocol_contracts
			.relayer_manager
			.as_ref()
			.unwrap()
			.heartbeat()
			.into_transaction_request()
	}

	/// Request send transaction to the target tx request channel.
	async fn request_send_transaction(
		&self,
		tx_request: N::TransactionRequest,
		metadata: HeartbeatMetadata,
	) {
		send_transaction(
			self.client.clone(),
			tx_request,
			SUB_LOG_TARGET.to_string(),
			Arc::new(metadata),
			self.debug_mode,
			self.handle.clone(),
		);
	}
}
