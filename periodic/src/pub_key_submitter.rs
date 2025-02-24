use std::{str::FromStr, sync::Arc, time::Duration};

use alloy::{
	network::AnyNetwork,
	primitives::{Address, Bytes},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	transports::Transport,
};
use bitcoincore_rpc::bitcoin::PublicKey;
use br_client::{
	btc::storage::keypair::{KeypairStorage, KeypairStorageT},
	eth::EthClient,
};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PUB_KEY_SUBMITTER_SCHEDULE},
	contracts::registration_pool::RegistrationPoolInstance,
	substrate::{
		EthereumSignature, MigrationSequence, Public, VaultKeySubmission,
		bifrost_runtime::{
			self, btc_registration_pool::storage::types::service_state::ServiceState,
		},
	},
	tx::{SubmitVaultKeyMetadata, XtRequest, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};
use cron::Schedule;
use eyre::Result;
use subxt::ext::subxt_core::utils::AccountId20;
use tokio::{sync::RwLock, time::sleep};
use tokio_stream::StreamExt;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "pubkey-submitter";

pub struct PubKeySubmitter<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The Bifrost client.
	pub client: Arc<EthClient<F, P, T>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: KeypairStorage,
	/// The migration sequence.
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	/// The time schedule that represents when check pending registrations.
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<F, P, T> PeriodicWorker for PubKeySubmitter<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		loop {
			self.wait_until_next_time().await;

			if self.is_relay_executive().await? {
				let target_round = if *self.migration_sequence.read().await == ServiceState::Normal
				{
					self.get_current_round().await?
				} else {
					// wait for 9 seconds to ensure the migration sequence is updated. (at least finalization time)
					sleep(Duration::from_secs(9)).await;
					self.get_current_round().await?.saturating_add(1)
				};

				let pending_registrations = self.get_pending_registrations(target_round).await?;

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

					let registration_info = self.get_registration_info(who, target_round).await?;

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

					let pub_key = self.keypair_storage.create_new_keypair().await;
					let (call, metadata) = self.build_unsigned_tx(who, pub_key).await?;
					self.request_send_transaction(call, metadata);
				}
			}
		}
	}
}

impl<F, P, T> PubKeySubmitter<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Instantiates a new `PubKeySubmitter` instance.
	pub fn new(
		client: Arc<EthClient<F, P, T>>,
		xt_request_sender: Arc<XtRequestSender>,
		keypair_storage: KeypairStorage,
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

	/// Build the payload for the unsigned transaction.
	/// (`submit_vault_key()` or `submit_system_vault_key()`)
	async fn build_payload(
		&self,
		who: Address,
		pub_key: PublicKey,
	) -> Result<(VaultKeySubmission<AccountId20>, EthereumSignature)> {
		let mut converted_pub_key = [0u8; 33];
		converted_pub_key.copy_from_slice(&pub_key.to_bytes());

		let pool_round = if *self.migration_sequence.read().await == ServiceState::Normal {
			self.get_current_round().await?
		} else {
			self.get_current_round().await?.saturating_add(1)
		};

		let msg = VaultKeySubmission {
			authority_id: AccountId20(self.client.address().0.0),
			who: AccountId20(who.0.0),
			pub_key: Public(converted_pub_key),
			pool_round,
		};
		let signature = self
			.client
			.sign_message(
				format!("{}:{}", pool_round, array_bytes::bytes2hex("0x", converted_pub_key))
					.as_bytes(),
			)
			.await?
			.into();

		Ok((msg, signature))
	}

	/// Build the calldata for the unsigned transaction.
	/// (`submit_vault_key()` or `submit_system_vault_key()`)
	async fn build_unsigned_tx(
		&self,
		who: Address,
		pub_key: PublicKey,
	) -> Result<(XtRequest, SubmitVaultKeyMetadata)> {
		let (msg, signature) = self.build_payload(who, pub_key).await?;
		let metadata = SubmitVaultKeyMetadata::new(who, pub_key);
		if self.is_system_vault(who) {
			Ok((
				XtRequest::from(
					bifrost_runtime::tx()
						.btc_registration_pool()
						.submit_system_vault_key(msg, signature),
				),
				metadata,
			))
		} else {
			Ok((
				XtRequest::from(
					bifrost_runtime::tx().btc_registration_pool().submit_vault_key(msg, signature),
				),
				metadata,
			))
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
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.client.address(),
					metadata,
					error
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
	async fn get_pending_registrations(&self, round: u32) -> Result<Vec<Address>> {
		Ok(self.registration_pool().pending_registrations(round).call().await?._0)
	}

	/// Get the user's registration information.
	async fn get_registration_info(
		&self,
		who: Address,
		round: u32,
	) -> Result<(Address, String, String, Vec<Address>, Vec<Bytes>)> {
		Ok(self.registration_pool().registration_info(who, round).call().await?.into())
	}

	fn registration_pool(&self) -> &RegistrationPoolInstance<F, P, T> {
		self.client.protocol_contracts.registration_pool.as_ref().unwrap()
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> Result<bool> {
		let relay_exec = self.client.protocol_contracts.relay_executive.as_ref().unwrap();
		Ok(relay_exec.is_member(self.client.address()).call().await?._0)
	}

	/// Verify whether the given address is a system vault.
	#[inline]
	fn is_system_vault(&self, who: Address) -> bool {
		who == *self.registration_pool().address()
	}

	async fn get_current_round(&self) -> Result<u32> {
		Ok(self.registration_pool().current_round().call().await?._0)
	}
}
