use alloy::{
	providers::{fillers::TxFiller, Provider, WalletProvider},
	rpc::types::TransactionRequest,
	transports::Transport,
};
use br_client::eth::{send_transaction, EthClient};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::HEARTBEAT_SCHEDULE},
	tx::{HeartbeatMetadata, TxRequestMetadata},
};
use cron::Schedule;
use eyre::Result;
use sc_service::SpawnTaskHandle;
use std::{str::FromStr, sync::Arc};

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "heartbeat-sender";

/// The essential task that sending heartbeat transaction.
pub struct HeartbeatSender<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// The time schedule that represents when to check heartbeat pulsed.
	schedule: Schedule,
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<F, P, T>>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
}

#[async_trait::async_trait]
impl<F, P, T> PeriodicWorker for HeartbeatSender<F, P, T>
where
	F: TxFiller + WalletProvider + 'static,
	P: Provider<T> + 'static,
	T: Transport + Clone,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		loop {
			let address = self.client.address();

			let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
			let is_selected = relayer_manager.is_selected_relayer(address, false).call().await?._0;
			let is_heartbeat_pulsed = relayer_manager.is_heartbeat_pulsed(address).call().await?._0;

			if is_selected && !is_heartbeat_pulsed {
				let round_info =
					self.client.protocol_contracts.authority.round_info().call().await?._0;
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

impl<F, P, T> HeartbeatSender<F, P, T>
where
	F: TxFiller + WalletProvider + 'static,
	P: Provider<T> + 'static,
	T: Transport + Clone,
{
	/// Instantiates a new `HeartbeatSender` instance.
	pub fn new(client: Arc<EthClient<F, P, T>>, handle: SpawnTaskHandle) -> Self {
		Self {
			schedule: Schedule::from_str(HEARTBEAT_SCHEDULE).expect(INVALID_PERIODIC_SCHEDULE),
			client,
			handle,
		}
	}

	/// Build `heartbeat` transaction.
	fn build_transaction(&self) -> TransactionRequest {
		let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
		TransactionRequest::default()
			.to(*relayer_manager.address())
			.input(relayer_manager.heartbeat().calldata().clone().into())
	}

	/// Request send transaction to the target tx request channel.
	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: HeartbeatMetadata,
	) {
		send_transaction(
			self.client.clone(),
			tx_request,
			SUB_LOG_TARGET.to_string(),
			TxRequestMetadata::Heartbeat(metadata),
			self.handle.clone(),
		);
	}
}
