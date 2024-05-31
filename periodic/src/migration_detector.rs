use crate::traits::PeriodicWorker;
use br_primitives::substrate::{bifrost_runtime, CustomConfig, MigrationSequence};
use std::str::FromStr;

use br_primitives::constants::schedule::MIGRATION_DETECTOR_SCHEDULE;
use cron::Schedule;
use std::sync::Arc;
use subxt::OnlineClient;
use tokio::sync::RwLock;

pub struct MigrationDetector {
	sub_client: OnlineClient<CustomConfig>,
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	schedule: Schedule,
}

impl MigrationDetector {
	pub fn new(
		sub_client: OnlineClient<CustomConfig>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
	) -> Self {
		Self {
			sub_client,
			migration_sequence,
			schedule: Schedule::from_str(MIGRATION_DETECTOR_SCHEDULE).unwrap(),
		}
	}
}

#[async_trait::async_trait]
impl PeriodicWorker for MigrationDetector {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			// Fetch the latest service state from the storage.
			{
				let service_state = self
					.sub_client
					.storage()
					.at_latest()
					.await
					.unwrap()
					.fetch(&bifrost_runtime::storage().btc_registration_pool().service_state())
					.await
					.unwrap()
					.unwrap();

				let mut write_lock = self.migration_sequence.write().await;
				*write_lock = service_state;
			}

			self.wait_until_next_time().await;
		}
	}
}
