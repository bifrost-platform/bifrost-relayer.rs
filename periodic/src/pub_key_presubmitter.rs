use crate::traits::PeriodicWorker;
use alloy::{
	network::Network,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use array_bytes::Hexify;
use bitcoincore_rpc::bitcoin::PublicKey;
use br_client::{
	btc::storage::keypair::{KeypairStorage, KeypairStorageT},
	eth::EthClient,
};
use br_primitives::{
	constants::{schedule::PRESUBMISSION_SCHEDULE, tx::DEFAULT_CALL_RETRY_INTERVAL_MS},
	substrate::{
		CustomConfig, EthereumSignature, MigrationSequence, Public, VaultKeyPreSubmission,
		bifrost_runtime::{
			self, btc_registration_pool::storage::types::service_state::ServiceState,
		},
		initialize_sub_client,
	},
	tx::{
		VaultKeyPresubmissionMetadata, XtRequest, XtRequestMessage, XtRequestMetadata,
		XtRequestSender,
	},
	utils::sub_display_format,
};
use cron::Schedule;
use eyre::Result;
use std::{str::FromStr, sync::Arc, time::Duration};
use subxt::{OnlineClient, ext::subxt_core::utils::AccountId20, storage::Storage};
use tokio::sync::RwLock;

const SUB_LOG_TARGET: &str = "pubkey-presubmitter";

pub struct PubKeyPreSubmitter<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub bfc_client: Arc<EthClient<F, P, N>>,
	/// The Bifrost client.
	sub_client: Option<OnlineClient<CustomConfig>>,
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
impl<F, P, N: Network> PeriodicWorker for PubKeyPreSubmitter<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		self.initialize().await;

		loop {
			if self.is_relay_executive().await? {
				if *self.migration_sequence.read().await != ServiceState::Normal {
					continue;
				}

				let n = self.get_presubmittable_amount().await;
				if n > 0 {
					log::info!(
						target: &self.bfc_client.get_chain_name(),
						"-[{}] {} presubmission available.",
						sub_display_format(SUB_LOG_TARGET),
						n,
					);

					let keys = self.create_pub_keys(n).await;
					let (call, metadata) = self.build_unsigned_tx(keys).await?;
					self.request_send_transaction(call, metadata).await;
				}
			}
			self.wait_until_next_time().await;
		}
	}
}

impl<F, P, N: Network> PubKeyPreSubmitter<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Instantiates a new `PubKeyPreSubmitter` instance.
	pub fn new(
		bfc_client: Arc<EthClient<F, P, N>>,
		xt_request_sender: Arc<XtRequestSender>,
		keypair_storage: KeypairStorage,
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
		self.sub_client = Some(initialize_sub_client(self.bfc_client.get_url()).await);
	}

	async fn build_unsigned_tx(
		&self,
		pub_keys: Vec<PublicKey>,
	) -> Result<(XtRequest, VaultKeyPresubmissionMetadata)> {
		let (msg, signature) = self.build_payload(&pub_keys).await?;
		let metadata = VaultKeyPresubmissionMetadata { keys: pub_keys.len() };

		Ok((
			Arc::new(
				bifrost_runtime::tx()
					.btc_registration_pool()
					.vault_key_presubmission(msg, signature),
			),
			metadata,
		))
	}

	async fn build_payload(
		&self,
		pub_keys: &[PublicKey],
	) -> Result<(VaultKeyPreSubmission<AccountId20>, EthereumSignature)> {
		let converted_pub_keys = pub_keys
			.iter()
			.map(|k| {
				let mut converted = [0u8; 33];
				converted.copy_from_slice(&k.to_bytes());
				converted
			})
			.collect::<Vec<[u8; 33]>>();

		let pool_round = self.get_current_round().await;
		let msg = VaultKeyPreSubmission {
			authority_id: AccountId20(self.bfc_client.address().await.0.0),
			pub_keys: converted_pub_keys.iter().map(|x| Public(*x)).collect(),
			pool_round,
		};
		let signature = self
			.bfc_client
			.sign_message(
				format!(
					"{}:{}",
					pool_round,
					converted_pub_keys
						.iter()
						.map(|x| x.hexify_prefixed())
						.collect::<Vec<String>>()
						.concat()
				)
				.as_bytes(),
			)
			.await?
			.into();

		Ok((msg, signature))
	}

	/// Send the transaction request message to the channel.
	async fn request_send_transaction(
		&self,
		call: XtRequest,
		metadata: VaultKeyPresubmissionMetadata,
	) {
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
				br_primitives::log_and_capture!(
					error,
					&self.bfc_client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to send unsigned transaction: {}, Error: {}",
					metadata,
					error
				);
			},
		}
	}

	/// Create public key * amount
	async fn create_pub_keys(&mut self, amount: usize) -> Vec<PublicKey> {
		let mut res = Vec::new();
		for _ in 0..amount {
			let key = self.keypair_storage.create_new_keypair().await;
			res.push(key);
		}
		res
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> Result<bool> {
		let relay_exec = self.bfc_client.protocol_contracts.relay_executive.as_ref().unwrap();
		Ok(relay_exec.is_member(self.bfc_client.address().await).call().await?)
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

	async fn get_presubmittable_amount(&self) -> usize {
		loop {
			let storage = self.get_latest_storage().await;
			let max_presubmission = storage
				.fetch(&bifrost_runtime::storage().btc_registration_pool().max_pre_submission())
				.await
				.unwrap()
				.unwrap() as usize;
			match storage
				.fetch(&bifrost_runtime::storage().btc_registration_pool().pre_submitted_pub_keys(
					self.get_current_round().await,
					AccountId20(self.bfc_client.address().await.0.0),
				))
				.await
			{
				Ok(Some(submitted)) => {
					return if submitted.len() > max_presubmission {
						0
					} else {
						max_presubmission - submitted.len()
					};
				},
				Ok(None) => return max_presubmission,
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
