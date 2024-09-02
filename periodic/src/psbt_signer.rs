use crate::traits::PeriodicWorker;
use br_client::{
	btc::{handlers::XtRequester, storage::keypair::KeypairStorage},
	eth::EthClient,
};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PSBT_SIGNER_SCHEDULE},
	substrate::{bifrost_runtime, MigrationSequence, SignedPsbtMessage},
	tx::{SubmitSignedPsbtMetadata, XtRequest, XtRequestMetadata, XtRequestSender},
	utils::{hash_bytes, sub_display_format},
};
use cron::Schedule;
use ethers::prelude::{Bytes, JsonRpcClient, H256};
use miniscript::bitcoin::{Address as BtcAddress, Network, Psbt};
use std::{str::FromStr, sync::Arc};
use tokio::sync::RwLock;
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "psbt-signer";

/// The essential task that submits signed PSBT's.
pub struct PsbtSigner<T> {
	/// The Bifrost client.
	client: Arc<EthClient<T>>,
	/// The extrinsic message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The public and private keypair local storage.
	keypair_storage: Arc<RwLock<KeypairStorage>>,
	/// The migration sequence.
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	/// The Bitcoin network.
	btc_network: Network,
	/// Loop schedule.
	schedule: Schedule,
}

impl<T: 'static + JsonRpcClient> PsbtSigner<T> {
	/// Instantiates a new `PsbtSigner` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender>,
		keypair_storage: Arc<RwLock<KeypairStorage>>,
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
	async fn get_unsigned_psbts(&self) -> Vec<Bytes> {
		let socket_queue = self.client.protocol_contracts.socket_queue.as_ref().unwrap();

		self.client
			.contract_call(socket_queue.unsigned_psbts(), "socket_queue.unsigned_psbts")
			.await
	}

	/// Verify whether the current relayer is an executive.
	async fn is_relay_executive(&self) -> bool {
		let relay_exec = self.client.protocol_contracts.relay_executive.as_ref().unwrap();

		self.client
			.contract_call(relay_exec.is_member(self.client.address()), "relay_executive.is_member")
			.await
	}

	/// Build the payload for the extrinsic. (`submit_signed_psbt()`)
	async fn build_payload(&self, unsigned_psbt: &mut Psbt) -> Option<SignedPsbtMessage> {
		match *self.migration_sequence.read().await {
			MigrationSequence::Normal => {},
			MigrationSequence::SetExecutiveMembers | MigrationSequence::PrepareNextSystemVault => {
				return None;
			},
			MigrationSequence::UTXOTransfer => {
				// Ensure that the extrinsic is for the system vault.
				let system_vault = self.get_system_vault(self.get_current_round().await + 1).await;
				if !unsigned_psbt.unsigned_tx.output.iter().all(|x| {
					BtcAddress::from_script(x.script_pubkey.as_script(), self.btc_network).unwrap()
						== system_vault
				}) {
					let log_msg = format!(
						"-[{}] ❕ Only transfer to new system vault is allowed on `UTXOTransfer` sequence: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						unsigned_psbt
					);
					log::warn!(target: &self.client.get_chain_name(), "{log_msg}");
					sentry::capture_message(
						&format!("[{}]{log_msg}", &self.client.get_chain_name()),
						sentry::Level::Warning,
					);

					return None;
				}
			},
		};

		let mut psbt = unsigned_psbt.clone();
		if self.keypair_storage.read().await.sign_psbt(&mut psbt) {
			let signed_psbt = psbt.serialize();

			if self
				.is_signed_psbt_submitted(
					H256::from_str(&psbt.unsigned_tx.txid().to_string()).unwrap(),
					signed_psbt.clone(),
				)
				.await
			{
				return None;
			}

			let msg = SignedPsbtMessage {
				unsigned_psbt: unsigned_psbt.serialize(),
				signed_psbt: signed_psbt.clone(),
			};
			return Some(msg);
		}
		log::warn!(
			target: &self.client.get_chain_name(),
			"-[{}] 🔐 Unauthorized to sign PSBT: {}",
			sub_display_format(SUB_LOG_TARGET),
			hash_bytes(&psbt.serialize())
		);
		None
	}

	/// Build the calldata for the extrinsic. (`submit_signed_psbt()`)
	async fn build_unsigned_tx(
		&self,
		unsigned_psbt: &mut Psbt,
	) -> Option<(XtRequest, SubmitSignedPsbtMetadata)> {
		if let Some(msg) = self.build_payload(unsigned_psbt).await {
			let metadata = SubmitSignedPsbtMetadata::new(hash_bytes(&msg.unsigned_psbt));
			return Some((
				XtRequest::from(bifrost_runtime::tx().btc_socket_queue().submit_signed_psbt(msg)),
				metadata,
			));
		}
		None
	}

	/// Get the system vault address.
	async fn get_system_vault(&self, round: u32) -> BtcAddress {
		let registration_pool = self.client.protocol_contracts.registration_pool.as_ref().unwrap();
		let system_vault = self
			.client
			.contract_call(
				registration_pool.vault_address(registration_pool.address(), round),
				"registration_pool.vault_address",
			)
			.await;

		BtcAddress::from_str(&system_vault).unwrap().assume_checked()
	}

	/// Get the current round number.
	async fn get_current_round(&self) -> u32 {
		let registration_pool = self.client.protocol_contracts.registration_pool.as_ref().unwrap();
		self.client
			.contract_call(registration_pool.current_round(), "registration_pool.current_round")
			.await
	}

	async fn is_signed_psbt_submitted(&self, txid: H256, psbt: Vec<u8>) -> bool {
		let socket_queue = self.client.protocol_contracts.socket_queue.as_ref().unwrap();
		self.client
			.contract_call(
				socket_queue.is_signed_psbt_submitted(
					txid.into(),
					psbt.into(),
					self.client.address(),
				),
				"socket_queue.is_signed_psbt_submitted",
			)
			.await
	}
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> XtRequester<T> for PsbtSigner<T> {
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> PeriodicWorker for PsbtSigner<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			self.wait_until_next_time().await;

			if self.is_relay_executive().await {
				let unsigned_psbts = self.get_unsigned_psbts().await;
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔐 {} unsigned PSBT exists.",
					sub_display_format(SUB_LOG_TARGET),
					unsigned_psbts.len()
				);

				let mut stream = tokio_stream::iter(unsigned_psbts);
				while let Some(unsigned_psbt) = stream.next().await {
					// Build the extrinsic.
					if let Some((call, metadata)) = self
						.build_unsigned_tx(&mut Psbt::deserialize(&unsigned_psbt).unwrap())
						.await
					{
						// Send the extrinsic.
						self.request_send_transaction(
							call,
							XtRequestMetadata::SubmitSignedPsbt(metadata),
							SUB_LOG_TARGET,
						);
					}
				}
			}
		}
	}
}
