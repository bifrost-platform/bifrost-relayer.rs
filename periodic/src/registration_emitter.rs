use cron::Schedule;
use ethers::types::Address;

use crate::traits::PeriodicWorker;
use br_client::bfc::SubClient;
use br_client::eth::EthClient;

use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::REGISTRATION_EMITTER_SCHEDULE},
	contracts::registration_pool::VaultPending,
	sub_display_format,
	tx::{
		SubmitVaultKeyMetadata, TxRequestSender, XtRequestMessage, XtRequestMetadata,
		XtRequestSender,
	},
};
use std::{str::FromStr, sync::Arc, time::Duration};
use subxt::tx::DynamicPayload;

use ethers::providers::JsonRpcClient;

const SUB_LOG_TARGET: &str = "registration-emitter";

pub struct RegistrationEmitter<T> {
	/// The ethereum client for the Bifrost network.
	sub_client: Arc<SubClient<T>>,
	/// The ethereum client for the Bifrost network.
	client: Arc<EthClient<T>>,
	/// The `XtRequestSender` for Bifrost.
	xt_request_sender: Arc<XtRequestSender<DynamicPayload>>,
	/// The time schedule that represents when to check round info.
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for RegistrationEmitter<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			self.wait_until_next_time().await;

			let pending_registrations = self.get_pending_registrations().await;

			if Some(pending_registrations) {
				let vault_pendings: Vec<VaultPending> = pending_registrations
					.iter()
					.map(|user_bfc_address, refund_address| VaultPending {
						user_bfc_address,
						refund_address,
					})
					.collect();

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 👤 registration detected. Total : {}",
					sub_display_format(SUB_LOG_TARGET),
					pending_registrations.len(),
				);

				if !self.is_selected_relayer().await {
					continue;
				}

				self.request_send_transactions(vault_pendings).await;
			}
		}
	}
}

impl<T: JsonRpcClient> RegistrationEmitter<T> {
	/// Instantiates a new `RegistrationEmitter` instance.
	pub fn new(
		sub_client: Arc<SubClient<T>>,
		client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender<DynamicPayload>>,
	) -> Self {
		Self {
			sub_client,
			client,
			xt_request_sender,
			schedule: Schedule::from_str(REGISTRATION_EMITTER_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
		}
	}

	/// Get the pending registrations.
	async fn get_pending_registrations(&self) -> Vec<(Address, String)> {
		self.client
			.contract_call(
				self.client.protocol_contracts.registration_pool.pending_registrations(),
				"registration_pool.pending_registrations",
			)
			.await
	}

	/// Request send transactions to the xt request channel.
	async fn request_send_transactions(&self, vault_pendings: Vec<VaultPending>) {
		let num_suceessd_submit = 0;

		for vault_pending in vault_pendings {
			let (pub_key, payload) = self
				.sub_client
				.build_vault_payload(self.client.address(), vault_pending.user_bfc_address)
				.await;

			let message: XtRequestMessage<_> = XtRequestMessage::new(
				payload,
				XtRequestMetadata::SubmitVaultKey(SubmitVaultKeyMetadata {
					who: vault_pending.user_bfc_address,
					key: pub_key,
				}),
			);

			match self.xt_request_sender.send(message) {
				Ok(_) => suceessd_submit += 1,
				Err(error) => log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to send vault key. User Bfc Address : {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					vault_pending.user_bfc_address,
					error.to_string()
				),
			}
		}
		if num_suceessd_submit == vault_pendings.len() {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 👤 Suceessfully submit all pending registration",
				sub_display_format(SUB_LOG_TARGET),
			)
		}
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self) -> bool {
		true
	}
}
