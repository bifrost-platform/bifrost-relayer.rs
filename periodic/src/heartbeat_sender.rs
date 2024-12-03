use alloy::{
	primitives::ChainId,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	rpc::types::TransactionRequest,
	transports::Transport,
};
use br_client::eth::EthClient;
use br_primitives::{
	constants::{
		errors::{INVALID_BIFROST_NATIVENESS, INVALID_PERIODIC_SCHEDULE},
		schedule::HEARTBEAT_SCHEDULE,
	},
	eth::GasCoefficient,
	tx::{HeartbeatMetadata, TxRequestMessage, TxRequestMetadata},
	utils::sub_display_format,
};
use cron::Schedule;
use eyre::Result;
use std::{collections::BTreeMap, str::FromStr, sync::Arc};

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
	client: Arc<EthClient<F, P, T>>,
	/// The clients to interact with external chains.
	clients: Arc<BTreeMap<ChainId, Arc<EthClient<F, P, T>>>>,
}

#[async_trait::async_trait]
impl<F, P, T> PeriodicWorker for HeartbeatSender<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
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
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// Instantiates a new `HeartbeatSender` instance.
	pub fn new(clients: Arc<BTreeMap<ChainId, Arc<EthClient<F, P, T>>>>) -> Self {
		let (_, client) = clients
			.iter()
			.find(|(_, client)| client.metadata.is_native)
			.expect(INVALID_BIFROST_NATIVENESS);

		Self {
			schedule: Schedule::from_str(HEARTBEAT_SCHEDULE).expect(INVALID_PERIODIC_SCHEDULE),
			client: client.clone(),
			clients,
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
		// match self.tx_request_sender.send(TxRequestMessage::new(
		// 	tx_request,
		// 	TxRequestMetadata::Heartbeat(metadata.clone()),
		// 	false,
		// 	false,
		// 	GasCoefficient::Low,
		// 	false,
		// )) {
		// 	Ok(()) => log::info!(
		// 		target: &self.client.get_chain_name(),
		// 		"-[{}] 💓 Request Heartbeat transaction: {}",
		// 		sub_display_format(SUB_LOG_TARGET),
		// 		metadata
		// 	),
		// 	Err(error) => log::error!(
		// 		target: &self.client.get_chain_name(),
		// 		"-[{}] ❗️ Failed to request Heartbeat transaction: {}, Error: {}",
		// 		sub_display_format(SUB_LOG_TARGET),
		// 		metadata,
		// 		error.to_string()
		// 	),
		// }
		todo!()
	}
}
