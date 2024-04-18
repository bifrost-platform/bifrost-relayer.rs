use std::{str::FromStr, sync::Arc};

use bitcoincore_rpc::bitcoin::PublicKey;
use br_client::{btc::storage::keypair::KeypairStorage, eth::EthClient};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PUB_KEY_SUBMITTER_SCHEDULE},
	contracts::registration_pool::RegistrationPoolContract,
	substrate::{bifrost_runtime, AccountId20, EthereumSignature, Public, VaultKeySubmission},
	tx::{SubmitVaultKeyMetadata, XtRequest, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::{convert_ethers_to_ecdsa_signature, sub_display_format},
};
use cron::Schedule;
use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{Address, Bytes},
};
use tokio_stream::StreamExt;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "pubkey-submitter";

pub struct PubKeySubmitter<T> {
	/// The Bifrost client.
	client: Arc<EthClient<T>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: KeypairStorage,
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

					let pub_key = self.keypair_storage.create_new_keypair().await;
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
		keypair_storage: KeypairStorage,
	) -> Self {
		Self {
			client,
			xt_request_sender,
			keypair_storage,
			schedule: Schedule::from_str(PUB_KEY_SUBMITTER_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
		}
	}

	/// Build the payload for the unsigned transaction.
	/// (`submit_vault_key()` or `submit_system_vault_key()`)
	fn build_payload(
		&self,
		who: Address,
		pub_key: PublicKey,
	) -> (VaultKeySubmission<AccountId20>, EthereumSignature) {
		let mut converted_pub_key = [0u8; 33];
		converted_pub_key.copy_from_slice(&pub_key.to_bytes());
		// submit public key
		let msg = VaultKeySubmission {
			authority_id: AccountId20(self.client.address().0),
			who: AccountId20(who.0),
			pub_key: Public(converted_pub_key),
		};
		let message = array_bytes::bytes2hex("0x", converted_pub_key);
		let signature =
			convert_ethers_to_ecdsa_signature(self.client.wallet.sign_message(&message.as_bytes()));
		(msg, signature)
	}

	/// Build the calldata for the unsigned transaction.
	/// (`submit_vault_key()` or `submit_system_vault_key()`)
	fn build_unsigned_tx(
		&self,
		who: Address,
		pub_key: PublicKey,
	) -> (XtRequest, SubmitVaultKeyMetadata) {
		let (msg, signature) = self.build_payload(who, pub_key);
		let metadata = SubmitVaultKeyMetadata::new(who, pub_key);
		if who == self.registration_pool().address() {
			(
				XtRequest::from(
					bifrost_runtime::tx()
						.btc_registration_pool()
						.submit_system_vault_key(msg, signature),
				),
				metadata,
			)
		} else {
			(
				XtRequest::from(
					bifrost_runtime::tx().btc_registration_pool().submit_vault_key(msg, signature),
				),
				metadata,
			)
		}
	}

	/// Send the transaction request message to the channel.
	fn request_send_transaction(&self, call: XtRequest, metadata: SubmitVaultKeyMetadata) {
		match self.xt_request_sender.send(XtRequestMessage::new(
			call.into(),
			XtRequestMetadata::SubmitVaultKey(metadata.clone()),
		)) {
			Ok(_) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	/// Get the pending registrations.
	async fn get_pending_registrations(&self) -> Vec<Address> {
		self.client
			.contract_call(
				self.registration_pool().pending_registrations(),
				"registration_pool.pending_registrations",
			)
			.await
			.0
	}

	/// Get the user's registration information.
	async fn get_registration_info(
		&self,
		who: Address,
	) -> (Address, String, String, Vec<Address>, Vec<Bytes>) {
		self.client
			.contract_call(
				self.registration_pool().registration_info(who),
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
}
