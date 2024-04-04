use std::{str::FromStr, sync::Arc};

use br_client::{btc::storage::keypair::KeypairStorage, eth::EthClient};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PUB_KEY_SUBMITTER_SCHEDULE},
	tx::XtRequestSender,
	utils::sub_display_format,
};
use cron::Schedule;
use ethers::{providers::JsonRpcClient, types::Address};
use tokio_stream::StreamExt;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "pubkey-submitter";

pub struct PubKeySubmitter<T> {
	/// The Bifrost client.
	client: Arc<EthClient<T>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: Arc<KeypairStorage>,
	/// The time schedule that represents when check pending registrations.
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static> PeriodicWorker for PubKeySubmitter<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			if self.is_relay_executive().await {
				let pending_registrations = self.get_pending_registrations().await;

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔐 Pending vault registrations: {:?}",
					sub_display_format(SUB_LOG_TARGET),
					pending_registrations.len()
				);

				let mut stream = tokio_stream::iter(pending_registrations);
				while let Some(who) = stream.next().await {
					let registration_info = self.get_registration_info(who).await;

					// user doesn't exist in the pool.
					if registration_info.0 != who {
						continue;
					}
					// the vault address is already generated.
					if !registration_info.2.is_empty() {
						continue;
					}
					// the relayer already submitted a public key.
					if registration_info.3.contains(&self.client.address()) {
						continue;
					}

					// create new keypair
					// submit pubkey
				}
			}

			self.wait_until_next_time().await;
		}
	}
}

impl<T: JsonRpcClient> PubKeySubmitter<T> {
	pub fn new(
		client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender>,
		keypair_storage: Arc<KeypairStorage>,
	) -> Self {
		Self {
			client,
			xt_request_sender,
			keypair_storage,
			schedule: Schedule::from_str(PUB_KEY_SUBMITTER_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
		}
	}

	async fn get_pending_registrations(&self) -> Vec<Address> {
		let registration_pool = self.client.protocol_contracts.registration_pool.as_ref().unwrap();

		self.client
			.contract_call(
				registration_pool.pending_registrations(),
				"registration_pool.pending_registrations",
			)
			.await
			.0
	}

	async fn get_registration_info(
		&self,
		who: Address,
	) -> (Address, String, String, Vec<Address>, Vec<String>) {
		let registration_pool = self.client.protocol_contracts.registration_pool.as_ref().unwrap();

		self.client
			.contract_call(
				registration_pool.registration_info(who),
				"registration_pool.registration_info",
			)
			.await
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> bool {
		let relay_exec = self.client.protocol_contracts.relay_executive.as_ref().unwrap();

		self.client
			.contract_call(relay_exec.is_member(self.client.address()), "relay_executive.is_member")
			.await
	}
}
