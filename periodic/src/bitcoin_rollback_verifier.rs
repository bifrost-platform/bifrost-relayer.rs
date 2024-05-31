use std::{str::FromStr, sync::Arc};

use br_client::eth::EthClient;
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::BITCOIN_ROLLBACK_CHECK_SCHEDULE},
	tx::XtRequestSender,
};
use cron::Schedule;
use ethers::providers::JsonRpcClient;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "rollback-verifier";

pub struct BitcoinRollbackVerifier<T> {
	client: Arc<EthClient<T>>,
	xt_request_sender: Arc<XtRequestSender>,
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for BitcoinRollbackVerifier<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {}
	}
}

impl<T: JsonRpcClient> BitcoinRollbackVerifier<T> {
	pub fn new(client: Arc<EthClient<T>>, xt_request_sender: Arc<XtRequestSender>) -> Self {
		Self {
			client,
			xt_request_sender,
			schedule: Schedule::from_str(BITCOIN_ROLLBACK_CHECK_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
		}
	}
}
