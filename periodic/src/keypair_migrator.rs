use crate::traits::PeriodicWorker;

use alloy::{
	network::AnyNetwork,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	transports::Transport,
};
use br_client::{btc::storage::keypair::KeypairStorage, eth::EthClient};
use br_primitives::{
	constants::{
		errors::INVALID_PROVIDER_URL, schedule::MIGRATION_DETECTOR_SCHEDULE,
		tx::DEFAULT_CALL_RETRY_INTERVAL_MS,
	},
	substrate::{
		bifrost_runtime::{
			self, btc_registration_pool::storage::types::service_state::ServiceState,
		},
		CustomConfig, MigrationSequence,
	},
};
use cron::Schedule;
use eyre::Result;
use std::{str::FromStr, sync::Arc, time::Duration};
use subxt::OnlineClient;
use tokio::sync::RwLock;

pub struct KeypairMigrator<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	sub_client: Option<OnlineClient<CustomConfig>>,
	bfc_client: Arc<EthClient<F, P, T>>,
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	keypair_storage: Arc<RwLock<KeypairStorage>>,
	schedule: Schedule,
}

impl<F, P, T> KeypairMigrator<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Instantiates a new `KeypairMigrator` instance.
	pub fn new(
		bfc_client: Arc<EthClient<F, P, T>>,
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
			url.set_scheme("wss").expect(INVALID_PROVIDER_URL);
		} else {
			url.set_scheme("ws").expect(INVALID_PROVIDER_URL);
		}

		self.sub_client = Some(
			OnlineClient::<CustomConfig>::from_insecure_url(url.as_str())
				.await
				.expect(INVALID_PROVIDER_URL),
		);

		match self.get_service_state().await {
			ServiceState::Normal
			| MigrationSequence::SetExecutiveMembers
			| ServiceState::UTXOTransfer => {
				self.keypair_storage.write().await.load(self.get_current_round().await).await;
			},
			ServiceState::PrepareNextSystemVault => {
				self.keypair_storage
					.write()
					.await
					.load(self.get_current_round().await + 1)
					.await;
			},
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
impl<F, P, T> PeriodicWorker for KeypairMigrator<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		self.initialize().await;

		loop {
			// Fetch the latest service state from the storage.
			{
				let service_state = self.get_service_state().await;

				let mut write_lock = self.migration_sequence.write().await;
				match *write_lock {
					MigrationSequence::Normal | MigrationSequence::SetExecutiveMembers => {
						if service_state == ServiceState::PrepareNextSystemVault {
							self.keypair_storage
								.write()
								.await
								.load(self.get_current_round().await + 1)
								.await;
						}
					},
					MigrationSequence::PrepareNextSystemVault => {
						if service_state == ServiceState::UTXOTransfer {
							self.keypair_storage
								.write()
								.await
								.load(self.get_current_round().await)
								.await;
						}
					},
					MigrationSequence::UTXOTransfer => {
						if service_state == ServiceState::Normal {
							self.keypair_storage
								.write()
								.await
								.load(self.get_current_round().await)
								.await;
						}
					},
				}
				*write_lock = service_state;
			}

			self.wait_until_next_time().await;
		}
	}
}
