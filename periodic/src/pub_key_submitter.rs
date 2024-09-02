use std::{str::FromStr, sync::Arc};

use bitcoincore_rpc::bitcoin::PublicKey;
use br_client::{btc::storage::keypair::KeypairStorage, eth::EthClient};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PUB_KEY_SUBMITTER_SCHEDULE},
	contracts::registration_pool::RegistrationPoolContract,
	substrate::{
		bifrost_runtime::{
			self, btc_registration_pool::storage::types::service_state::ServiceState,
		},
		AccountId20, MigrationSequence, Public, VaultKeySubmission,
	},
	tx::{SubmitVaultKeyMetadata, XtRequest, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};
use cron::Schedule;
use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{Address, Bytes},
};
use tokio::sync::RwLock;
use tokio_stream::StreamExt;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "pubkey-submitter";

pub struct PubKeySubmitter<T> {
	/// The Bifrost client.
	client: Arc<EthClient<T>>,
	/// The extrinsic message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: Arc<RwLock<KeypairStorage>>,
	/// The migration sequence.
	migration_sequence: Arc<RwLock<MigrationSequence>>,
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
			self.wait_until_next_time().await;

			if self.is_relay_executive().await {
				let target_round = if *self.migration_sequence.read().await == ServiceState::Normal
				{
					self.get_current_round().await
				} else {
					self.get_current_round().await.saturating_add(1)
				};

				let pending_registrations = self.get_pending_registrations(target_round).await;

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔐 Pending vault registrations: {:?}",
					sub_display_format(SUB_LOG_TARGET),
					pending_registrations.len()
				);

				let mut stream = tokio_stream::iter(pending_registrations);
				while let Some(who) = stream.next().await {
					// Skip the registration if the service is in maintenance mode. (Only system vaults are allowed to register in maintenance mode.)
					if *self.migration_sequence.read().await != ServiceState::Normal
						&& !self.is_system_vault(who)
					{
						continue;
					}

					let registration_info = self.get_registration_info(who, target_round).await;

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

					let pub_key = self.keypair_storage.write().await.create_new_keypair().await;
					let (call, metadata) = self.build_unsigned_tx(who, pub_key);
					self.request_send_transaction(call, metadata);
				}
			}
		}
	}
}

impl<T: JsonRpcClient> PubKeySubmitter<T> {
	/// Instantiates a new `PubKeySubmitter` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender>,
		keypair_storage: Arc<RwLock<KeypairStorage>>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
	) -> Self {
		Self {
			client,
			xt_request_sender,
			keypair_storage,
			migration_sequence,
			schedule: Schedule::from_str(PUB_KEY_SUBMITTER_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
		}
	}

	/// Build the payload for the extrinsic.
	/// (`submit_vault_key()` or `submit_system_vault_key()`)
	fn build_payload(
		&self,
		who: Address,
		pub_key: PublicKey,
	) -> VaultKeySubmission<bifrost_runtime::runtime_types::fp_account::AccountId20> {
		let mut converted_pub_key = [0u8; 33];
		converted_pub_key.copy_from_slice(&pub_key.to_bytes());
		VaultKeySubmission { who: AccountId20(who.0).into(), pub_key: Public(converted_pub_key) }
	}

	/// Build the calldata for the extrinsic.
	/// (`submit_vault_key()` or `submit_system_vault_key()`)
	fn build_unsigned_tx(
		&self,
		who: Address,
		pub_key: PublicKey,
	) -> (XtRequest, SubmitVaultKeyMetadata) {
		let msg = self.build_payload(who, pub_key);
		let metadata = SubmitVaultKeyMetadata::new(who, pub_key);
		if self.is_system_vault(who) {
			(
				XtRequest::from(
					bifrost_runtime::tx().btc_registration_pool().submit_system_vault_key(msg),
				),
				metadata,
			)
		} else {
			(
				XtRequest::from(
					bifrost_runtime::tx().btc_registration_pool().submit_vault_key(msg),
				),
				metadata,
			)
		}
	}

	/// Send the transaction request message to the channel.
	fn request_send_transaction(&self, call: XtRequest, metadata: SubmitVaultKeyMetadata) {
		match self
			.xt_request_sender
			.send(XtRequestMessage::new(call, XtRequestMetadata::SubmitVaultKey(metadata.clone())))
		{
			Ok(_) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔖 Request extrinsic: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to send extrinsic: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.client.address(),
					metadata,
					error.to_string()
				);
				log::error!(target: &self.client.get_chain_name(), "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", &self.client.get_chain_name()),
					sentry::Level::Error,
				);
			},
		}
	}

	/// Get the pending registrations.
	async fn get_pending_registrations(&self, round: u32) -> Vec<Address> {
		self.client
			.contract_call(
				self.registration_pool().pending_registrations(round),
				"registration_pool.pending_registrations",
			)
			.await
			.0
	}

	/// Get the user's registration information.
	async fn get_registration_info(
		&self,
		who: Address,
		round: u32,
	) -> (Address, String, String, Vec<Address>, Vec<Bytes>) {
		self.client
			.contract_call(
				self.registration_pool().registration_info(who, round),
				"registration_pool.registration_info",
			)
			.await
	}

	fn registration_pool(&self) -> &RegistrationPoolContract<Provider<T>> {
		self.client.protocol_contracts.registration_pool.as_ref().unwrap()
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> bool {
		let relay_exec = self.client.protocol_contracts.relay_executive.as_ref().unwrap();

		self.client
			.contract_call(relay_exec.is_member(self.client.address()), "relay_executive.is_member")
			.await
	}

	/// Verify whether the given address is a system vault.
	#[inline]
	fn is_system_vault(&self, who: Address) -> bool {
		who == self.registration_pool().address()
	}

	async fn get_current_round(&self) -> u32 {
		let registration_pool = self.registration_pool();
		self.client
			.contract_call(registration_pool.current_round(), "registration_pool.current_round")
			.await
	}
}
