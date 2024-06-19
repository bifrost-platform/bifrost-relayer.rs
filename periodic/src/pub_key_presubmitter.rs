use crate::traits::PeriodicWorker;
use bitcoincore_rpc::bitcoin::PublicKey;
use br_client::{btc::storage::keypair::KeypairStorage, eth::EthClient};
use br_primitives::{
	constants::{
		errors::INVALID_PROVIDER_URL, schedule::PRESUBMISSION_SCHEDULE,
		tx::DEFAULT_CALL_RETRY_INTERVAL_MS,
	},
	substrate::{
		bifrost_runtime,
		bifrost_runtime::btc_registration_pool::storage::types::service_state::ServiceState,
		AccountId20, CustomConfig, EthereumSignature, MigrationSequence, Public,
		VaultKeyPreSubmission,
	},
	tx::{
		VaultKeyPresubmissionMetadata, XtRequest, XtRequestMessage, XtRequestMetadata,
		XtRequestSender,
	},
	utils::{convert_ethers_to_ecdsa_signature, sub_display_format},
};
use cron::Schedule;
use ethers::prelude::JsonRpcClient;
use std::{str::FromStr, sync::Arc, time::Duration};
use subxt::{storage::Storage, OnlineClient};
use tokio::sync::RwLock;

const SUB_LOG_TARGET: &str = "pubkey-presubmitter";

pub struct PubKeyPreSubmitter<T> {
	bfc_client: Arc<EthClient<T>>,
	/// The Bifrost client.
	sub_client: Option<OnlineClient<CustomConfig>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: Arc<RwLock<KeypairStorage>>,
	/// The migration sequence.
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	/// The time schedule that represents when check pending registrations.
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static> PeriodicWorker for PubKeyPreSubmitter<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		self.initialize().await;

		loop {
			self.wait_until_next_time().await;

			if self.is_relay_executive().await {
				if *self.migration_sequence.read().await != ServiceState::Normal {
					continue;
				}

				let n = self.get_n().await;
				if n > 0 {
					log::info!(
						target: &self.bfc_client.get_chain_name(),
						"-[{}] {} presubmission available.",
						sub_display_format(SUB_LOG_TARGET),
						n,
					);

					let (call, metadata) = self.build_unsigned_tx(self.get_n_pub_keys(n).await);
					self.request_send_transaction(call, metadata);
				}
			}
		}
	}
}

impl<T: JsonRpcClient + 'static> PubKeyPreSubmitter<T> {
	/// Instantiates a new `PubKeyPreSubmitter` instance.
	pub fn new(
		bfc_client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender>,
		keypair_storage: Arc<RwLock<KeypairStorage>>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
	) -> Self {
		Self {
			bfc_client,
			sub_client: None,
			xt_request_sender,
			keypair_storage,
			migration_sequence,
			schedule: Schedule::from_str(PRESUBMISSION_SCHEDULE).unwrap(),
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
			OnlineClient::<CustomConfig>::from_url(url.as_str())
				.await
				.expect(INVALID_PROVIDER_URL),
		);
	}

	fn build_unsigned_tx(
		&self,
		pub_keys: Vec<PublicKey>,
	) -> (XtRequest, VaultKeyPresubmissionMetadata) {
		let (msg, signature) = self.build_payload(&pub_keys);
		let metadata = VaultKeyPresubmissionMetadata { keys: pub_keys.len() };

		(
			XtRequest::from(
				bifrost_runtime::tx()
					.btc_registration_pool()
					.vault_key_presubmission(msg, signature),
			),
			metadata,
		)
	}

	fn build_payload(
		&self,
		pub_keys: &Vec<PublicKey>,
	) -> (VaultKeyPreSubmission<AccountId20>, EthereumSignature) {
		let converted_pub_keys = pub_keys
			.iter()
			.map(|k| {
				let mut converted = [0u8; 33];
				converted.copy_from_slice(&k.to_bytes());
				converted
			})
			.collect::<Vec<[u8; 33]>>();

		let msg = VaultKeyPreSubmission {
			authority_id: AccountId20(self.bfc_client.address().0),
			pub_keys: converted_pub_keys.iter().map(|x| Public(*x)).collect(),
		};
		let signature = convert_ethers_to_ecdsa_signature(
			self.bfc_client.wallet.sign_message(
				&converted_pub_keys
					.iter()
					.map(|x| array_bytes::bytes2hex("0x", *x))
					.collect::<Vec<String>>()
					.concat()
					.as_bytes(),
			),
		);

		(msg, signature)
	}

	/// Send the transaction request message to the channel.
	fn request_send_transaction(&self, call: XtRequest, metadata: VaultKeyPresubmissionMetadata) {
		match self.xt_request_sender.send(XtRequestMessage::new(
			call,
			XtRequestMetadata::VaultKeyPresubmission(metadata.clone()),
		)) {
			Ok(_) => log::info!(
				target: &self.bfc_client.get_chain_name(),
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.bfc_client.address(),
					metadata,
					error.to_string()
				);
				log::error!(target: &self.bfc_client.get_chain_name(), "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", &self.bfc_client.get_chain_name()),
					sentry::Level::Error,
				);
			},
		}
	}

	/// Create n public keys.
	async fn get_n_pub_keys(&self, n: usize) -> Vec<PublicKey> {
		let mut res = Vec::new();
		let mut keypair_storage = self.keypair_storage.write().await;
		for _ in 0..n {
			let key = keypair_storage.create_new_keypair().await;
			res.push(key);
		}
		res
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> bool {
		let relay_exec = self.bfc_client.protocol_contracts.relay_executive.as_ref().unwrap();

		self.bfc_client
			.contract_call(
				relay_exec.is_member(self.bfc_client.address()),
				"relay_executive.is_member",
			)
			.await
	}

	async fn get_latest_storage(&self) -> Storage<CustomConfig, OnlineClient<CustomConfig>> {
		loop {
			match self.sub_client.as_ref().unwrap().storage().at_latest().await {
				Ok(storage) => return storage,
				Err(_) => {
					tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					continue;
				},
			}
		}
	}

	async fn get_n(&self) -> usize {
		loop {
			let storage = self.get_latest_storage().await;
			match storage
				.fetch(&bifrost_runtime::storage().btc_registration_pool().pre_submitted_pub_keys(
					self.get_current_round().await,
					AccountId20(self.bfc_client.address().0),
				))
				.await
			{
				Ok(Some(submitted)) => {
					let max_presubmission = storage
						.fetch(
							&bifrost_runtime::storage()
								.btc_registration_pool()
								.max_pre_submission(),
						)
						.await
						.unwrap()
						.unwrap();

					return max_presubmission as usize - submitted.len();
				},
				Ok(None) => {
					unreachable!("Should always be available.")
				},
				Err(_) => {
					tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
				},
			}
		}
	}

	/// Get the current round number.
	async fn get_current_round(&self) -> u32 {
		loop {
			let storage = self.get_latest_storage().await;
			match storage
				.fetch(&bifrost_runtime::storage().btc_registration_pool().current_round())
				.await
			{
				Ok(Some(round)) => return round,
				Ok(None) => {
					unreachable!("The current round number should always be available.")
				},
				Err(_) => {
					tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
				},
			}
		}
	}
}
