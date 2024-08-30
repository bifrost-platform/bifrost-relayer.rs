use crate::traits::PeriodicWorker;

use br_client::{btc::storage::keypair::KeypairStorage, eth::EthClient};
use br_primitives::{
	constants::{schedule::MIGRATION_DETECTOR_SCHEDULE, tx::DEFAULT_CALL_RETRY_INTERVAL_MS},
	substrate::{
		bifrost_runtime::{
			self, btc_registration_pool::storage::types::service_state::ServiceState,
		},
		CustomConfig, MigrationSequence,
	},
};
use cron::Schedule;
use ethers::prelude::JsonRpcClient;
use std::{str::FromStr, sync::Arc, time::Duration};
use subxt::OnlineClient;
use tokio::sync::RwLock;

pub struct KeypairMigrator<T> {
	sub_client: Option<OnlineClient<CustomConfig>>,
	bfc_client: Arc<EthClient<T>>,
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	keypair_storage: Arc<RwLock<KeypairStorage>>,
	schedule: Schedule,
}

impl<T: JsonRpcClient> KeypairMigrator<T> {
	/// Instantiates a new `KeypairMigrator` instance.
	pub fn new(
		bfc_client: Arc<EthClient<T>>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		keypair_storage: Arc<RwLock<KeypairStorage>>,
	) -> Self {
		Self {
			sub_client: None,
			bfc_client,
			migration_sequence,
			keypair_storage,
			schedule: Schedule::from_str(MIGRATION_DETECTOR_SCHEDULE).unwrap(),
		}
	}

	async fn initialize(&mut self) {
		let mut url = self.bfc_client.get_url();
		if url.scheme() == "https" {
			url.set_scheme("wss").unwrap();
		} else {
			url.set_scheme("ws").unwrap();
		}

		self.sub_client = Some(OnlineClient::<CustomConfig>::from_url(url.as_str()).await.unwrap());

		match self.get_service_state().await {
			ServiceState::Normal | ServiceState::UTXOTransfer => {
				self.keypair_storage.write().await.load(self.get_current_round().await).await;
			},
			ServiceState::PrepareNextSystemVault => {
				self.keypair_storage
					.write()
					.await
					.load(self.get_current_round().await + 1)
					.await;
			},
			_ => {},
		}
	}

	/// Get the current round number.
	async fn get_current_round(&self) -> u32 {
		loop {
			match self.sub_client.as_ref().unwrap().storage().at_latest().await {
				Ok(storage) => {
					match storage
						.fetch(&bifrost_runtime::storage().btc_registration_pool().current_round())
						.await
					{
						Ok(Some(round)) => return round,
						Ok(None) => {
							unreachable!("The current round number should always be available.")
						},
						Err(_) => {
							tokio::time::sleep(Duration::from_millis(
								DEFAULT_CALL_RETRY_INTERVAL_MS,
							))
							.await;
							continue;
						},
					}
				},
				Err(_) => {
					tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					continue;
				},
			}
		}
	}

	/// Fetch the latest service state from the storage.
	async fn get_service_state(&self) -> ServiceState {
		loop {
			match self.sub_client.as_ref().unwrap().storage().at_latest().await {
				Ok(storage) => {
					match storage
						.fetch(&bifrost_runtime::storage().btc_registration_pool().service_state())
						.await
					{
						Ok(Some(state)) => return state,
						Ok(None) => {
							unreachable!("The service state should always be available.")
						},
						Err(_) => {
							tokio::time::sleep(Duration::from_millis(
								DEFAULT_CALL_RETRY_INTERVAL_MS,
							))
							.await;
							continue;
						},
					}
				},
				Err(_) => {
					tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					continue;
				},
			}
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for KeypairMigrator<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		self.initialize().await;

		loop {
			// Fetch the latest service state from the storage.
			{
				let service_state = self.get_service_state().await;

				let mut write_lock = self.migration_sequence.write().await;
				match *write_lock {
					MigrationSequence::Normal => match service_state {
						ServiceState::PrepareNextSystemVault => {
							self.keypair_storage
								.write()
								.await
								.load(self.get_current_round().await + 1)
								.await;
						},
						_ => {},
					},
					MigrationSequence::PrepareNextSystemVault => match service_state {
						ServiceState::UTXOTransfer => {
							self.keypair_storage
								.write()
								.await
								.load(self.get_current_round().await)
								.await;
						},
						_ => {},
					},
					MigrationSequence::UTXOTransfer => match service_state {
						ServiceState::Normal => {
							self.keypair_storage
								.write()
								.await
								.load(self.get_current_round().await)
								.await;
						},
						_ => {},
					},
					_ => {},
				}
				*write_lock = service_state;
			}

			self.wait_until_next_time().await;
		}
	}
}
