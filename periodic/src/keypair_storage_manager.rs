use crate::traits::PeriodicWorker;
use br_primitives::substrate::{bifrost_runtime, CustomConfig, MigrationSequence};
use std::str::FromStr;

use br_client::eth::EthClient;
use br_primitives::constants::schedule::MIGRATION_DETECTOR_SCHEDULE;
use cron::Schedule;
use ethers::prelude::JsonRpcClient;
use std::sync::Arc;
use subxt::OnlineClient;
use tokio::sync::RwLock;
use br_client::btc::storage::keypair::KeypairStorage;
use br_primitives::substrate::bifrost_runtime::btc_registration_pool::storage::types::service_state::ServiceState;

pub struct KeypairStorageManager<T> {
	sub_client: Option<OnlineClient<CustomConfig>>,
	bfc_client: Arc<EthClient<T>>,
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	keypair_storage: KeypairStorage,
	schedule: Schedule,
}

impl<T: JsonRpcClient> KeypairStorageManager<T> {
	pub fn new(
		bfc_client: Arc<EthClient<T>>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		keypair_storage: KeypairStorage,
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
			ServiceState::Normal => {
				self.keypair_storage.load(self.get_current_round().await).await;
			},
			_ => {
				self.keypair_storage.load(self.get_current_round().await + 1).await;
			},
		}
	}

	#[inline]
	async fn get_current_round(&self) -> u32 {
		self.sub_client
			.as_ref()
			.unwrap()
			.storage()
			.at_latest()
			.await
			.unwrap()
			.fetch(&bifrost_runtime::storage().btc_registration_pool().current_round())
			.await
			.unwrap()
			.unwrap()
	}

	#[inline]
	async fn get_service_state(&self) -> ServiceState {
		self.sub_client
			.as_ref()
			.unwrap()
			.storage()
			.at_latest()
			.await
			.unwrap()
			.fetch(&bifrost_runtime::storage().btc_registration_pool().service_state())
			.await
			.unwrap()
			.unwrap()
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for KeypairStorageManager<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		self.initialize().await;

		loop {
			let sub_client = self.sub_client.as_ref().unwrap();

			// Fetch the latest service state from the storage.
			{
				let service_state = sub_client
					.storage()
					.at_latest()
					.await
					.unwrap()
					.fetch(&bifrost_runtime::storage().btc_registration_pool().service_state())
					.await
					.unwrap()
					.unwrap();

				let mut write_lock = self.migration_sequence.write().await;
				match *write_lock {
					MigrationSequence::Normal => match service_state {
						ServiceState::PrepareNextSystemVault => {
							self.keypair_storage.load(self.get_current_round().await + 1).await;
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
