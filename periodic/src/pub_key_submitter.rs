use std::sync::Arc;

use br_client::{btc::storage::keypair::KeypairStorage, eth::EthClient};
use br_primitives::tx::XtRequestSender;
use cron::Schedule;
use ethers::providers::JsonRpcClient;

use crate::traits::PeriodicWorker;

pub struct PubKeySubmitter<T> {
	/// The Bifrost client.
	client: Arc<EthClient<T>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: Arc<KeypairStorage>,
	/// The time schedule that represents when to send price feeds.
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static> PeriodicWorker for PubKeySubmitter<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			self.wait_until_next_time().await;
		}
	}
}
