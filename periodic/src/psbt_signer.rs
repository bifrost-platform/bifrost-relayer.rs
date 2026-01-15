use crate::traits::PeriodicWorker;
use alloy::{
	network::Network as AlloyNetwork,
	primitives::{B256, Bytes, keccak256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use br_client::{
	btc::{
		handlers::XtRequester,
		storage::keypair::{KeypairStorage, KeypairStorageT},
	},
	eth::EthClient,
};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PSBT_SIGNER_SCHEDULE},
	substrate::{EthereumSignature, MigrationSequence, SignedPsbtMessage, bifrost_runtime},
	tx::{SubmitSignedPsbtMetadata, XtRequest, XtRequestMetadata, XtRequestSender},
	utils::{hash_bytes, sub_display_format},
};
use cron::Schedule;
use eyre::Result;
use miniscript::bitcoin::{Address as BtcAddress, Network, Psbt};
use std::{str::FromStr, sync::Arc};
use subxt::ext::subxt_core::utils::AccountId20;
use tokio::sync::RwLock;

const SUB_LOG_TARGET: &str = "psbt-signer";

/// The essential task that submits signed PSBT's.
pub struct PsbtSigner<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The Bifrost client.
	pub client: Arc<EthClient<F, P, N>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: KeypairStorage,
	/// The migration sequence.
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	/// The Bitcoin network.
	btc_network: Network,
	/// Loop schedule.
	schedule: Schedule,
}

impl<F, P, N: AlloyNetwork> PsbtSigner<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Instantiates a new `PsbtSigner` instance.
	pub fn new(
		client: Arc<EthClient<F, P, N>>,
		xt_request_sender: Arc<XtRequestSender>,
		keypair_storage: KeypairStorage,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		btc_network: Network,
	) -> Self {
		Self {
			client,
			xt_request_sender,
			keypair_storage,
			migration_sequence,
			btc_network,
			schedule: Schedule::from_str(PSBT_SIGNER_SCHEDULE).expect(INVALID_PERIODIC_SCHEDULE),
		}
	}

	/// Get the pending unsigned PSBT's (in bytes)
	async fn get_unsigned_psbts(&self) -> Result<Vec<Bytes>> {
		let socket_queue = self.client.protocol_contracts.socket_queue.as_ref().unwrap();
		let res = socket_queue.unsigned_psbts().call().await?;
		Ok(res)
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> Result<bool> {
		let relay_exec = self.client.protocol_contracts.relay_executive.as_ref().unwrap();
		Ok(relay_exec.is_member(self.client.address().await).call().await?)
	}

	/// Build the payload for the unsigned transaction. (`submit_signed_psbt()`)
	async fn build_payload(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Result<Option<(SignedPsbtMessage<AccountId20>, EthereumSignature)>> {
		match *self.migration_sequence.read().await {
			MigrationSequence::Normal => {},
			MigrationSequence::SetExecutiveMembers | MigrationSequence::PrepareNextSystemVault => {
				return Ok(None);
			},
			MigrationSequence::UTXOTransfer => {
				// Ensure that the unsigned transaction is for the system vault.
				let system_vault =
					self.get_system_vault(self.get_current_round().await? + 1).await?;
				if !unsigned_psbt.unsigned_tx.output.iter().all(|x| {
					BtcAddress::from_script(x.script_pubkey.as_script(), self.btc_network).unwrap()
						== system_vault
				}) {
					br_primitives::log_and_capture!(
						warn,
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						"❕ Only transfer to new system vault is allowed on `UTXOTransfer` sequence: {:?}",
						unsigned_psbt
					);

					return Ok(None);
				}
			},
		};

		let mut psbt = unsigned_psbt.clone();
		if self.keypair_storage.sign_psbt(&mut psbt).await {
			let signed_psbt = psbt.serialize();

			if self
				.is_signed_psbt_submitted(
					B256::from_str(&psbt.unsigned_tx.compute_txid().to_string()).unwrap(),
					signed_psbt.clone(),
				)
				.await?
			{
				return Ok(None);
			}

			let msg = SignedPsbtMessage {
				authority_id: AccountId20(self.client.address().await.0.0),
				unsigned_psbt: unsigned_psbt.serialize(),
				signed_psbt: signed_psbt.clone(),
			};
			let signature = self
				.client
				.sign_message(&[keccak256("SignedPsbt").as_slice(), signed_psbt.as_ref()].concat())
				.await?
				.into();
			return Ok(Some((msg, signature)));
		}
		log::warn!(
			target: &self.client.get_chain_name(),
			"-[{}] 🔐 Unauthorized to sign PSBT: {}",
			sub_display_format(SUB_LOG_TARGET),
			hash_bytes(&psbt.serialize())
		);
		Ok(None)
	}

	/// Build the calldata for the unsigned transaction. (`submit_signed_psbt()`)
	async fn build_unsigned_tx(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Result<Option<(XtRequest, SubmitSignedPsbtMetadata)>> {
		if let Some((msg, signature)) = self.build_payload(unsigned_psbt).await? {
			let metadata = SubmitSignedPsbtMetadata::new(hash_bytes(&msg.unsigned_psbt));
			return Ok(Some((
				XtRequest::from(
					bifrost_runtime::tx().btc_socket_queue().submit_signed_psbt(msg, signature),
				),
				metadata,
			)));
		}
		Ok(None)
	}

	/// Get the system vault address.
	async fn get_system_vault(&self, round: u32) -> Result<BtcAddress> {
		let registration_pool = self.client.protocol_contracts.registration_pool.as_ref().unwrap();
		let system_vault = registration_pool
			.vault_address(*registration_pool.address(), round)
			.call()
			.await?;

		Ok(BtcAddress::from_str(&system_vault)?.assume_checked())
	}

	/// Get the current round number.
	async fn get_current_round(&self) -> Result<u32> {
		let registration_pool = self.client.protocol_contracts.registration_pool.as_ref().unwrap();
		Ok(registration_pool.current_round().call().await?)
	}

	async fn is_signed_psbt_submitted(&self, txid: B256, psbt: Vec<u8>) -> Result<bool> {
		let socket_queue = self.client.protocol_contracts.socket_queue.as_ref().unwrap();
		let res = socket_queue
			.is_signed_psbt_submitted(txid, Bytes::from(psbt), self.client.address().await)
			.call()
			.await?;
		Ok(res)
	}
}

#[async_trait::async_trait]
impl<F, P, N: AlloyNetwork> XtRequester<F, P, N> for PsbtSigner<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<F, P, N>> {
		self.client.clone()
	}
}

#[async_trait::async_trait]
impl<F, P, N: AlloyNetwork> PeriodicWorker for PsbtSigner<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		loop {
			self.wait_until_next_time().await;

			if self.is_relay_executive().await? {
				let unsigned_psbts = self.get_unsigned_psbts().await?;
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔐 {} unsigned PSBT exists.",
					sub_display_format(SUB_LOG_TARGET),
					unsigned_psbts.len()
				);

				for unsigned_psbt in unsigned_psbts {
					// Build the unsigned transaction.
					if let Some((call, metadata)) = self
						.build_unsigned_tx(&mut Psbt::deserialize(&unsigned_psbt).unwrap())
						.await?
					{
						// Send the unsigned transaction.
						self.request_send_transaction(
							call,
							XtRequestMetadata::SubmitSignedPsbt(metadata),
							SUB_LOG_TARGET,
						)
						.await;
					}
				}
			}
		}
	}
}
