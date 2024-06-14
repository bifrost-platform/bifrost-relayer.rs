use serde::Deserialize;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{Barrier, Mutex, RwLock};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	eth::{BootstrapState, ChainID, GasCoefficient},
	tx::SolanaTxRequestMessage,
	utils::sub_display_format,
};
use sc_service::SpawnTaskHandle;

use solana_sdk::{
	commitment_config::CommitmentConfig,
	program_pack::Pack,
	signature::Keypair,
	signer::Signer,
	signers::Signers,
	system_instruction,
	transaction::{
		Transaction,
		TransactionError::{self},
	},
};

// use solana_rpc_client::nonblocking::rpc_client::RpcClient;
// use solana_rpc_client::rpc_client::SerializableTransaction;
// use solana_rpc_client_api::config::RpcSendTransactionConfig;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_client::SerializableTransaction;
use solana_client::rpc_config::RpcSendTransactionConfig;

use spl_token::{
	id, instruction,
	state::{Account, Mint},
};

use crate::solana::{event::EventManager as SOLEventManager, LOG_TARGET};

const SUB_LOG_TARGET: &str = "solana-tx-manager";
const SOLANA_URL: &str = "https://api.devnet.solana.com";

// The essential task that sends transactions asynchronously.
pub struct SolanaTransactionManager {
	/// The solana client.
	sol_client: Arc<RpcClient>,
	/// The receiver connected to the tx request channel.
	receiver: UnboundedReceiver<SolanaTxRequestMessage>,
	/// A handle for spawning transaction tasks in the service.
	sol_tx_spawn_handle: SpawnTaskHandle,
	pub payer: Keypair,
}

impl SolanaTransactionManager {
	/// Instantiates a new `UnsignedTransactionManager`.
	pub fn new(
		sol_client: Arc<RpcClient>,
		sol_tx_spawn_handle: SpawnTaskHandle,
		payer: Keypair,
	) -> (Self, UnboundedSender<SolanaTxRequestMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<SolanaTxRequestMessage>();
		(Self { sol_client, receiver, sol_tx_spawn_handle, payer }, sender)
	}

	fn get_client(&self) -> Arc<RpcClient> {
		self.sol_client.clone()
	}

	fn get_spawn_handle(&self) -> SpawnTaskHandle {
		self.sol_tx_spawn_handle.clone()
	}

	fn get_payer(&self) -> Keypair {
		let binding = self.payer.secret();
		let secret = &binding.as_ref();
		Keypair::from_bytes(&secret).expect("Failed to create Keypair from bytes")
	}

	/// Starts the transaction manager. Listens to every new consumed tx request message.
	pub async fn run(&mut self) {
		while let Some(msg) = self.receiver.recv().await {
			log::info!(
				target: "SOLANA",
				"-[{}] 🔖 Received unsigned transaction request: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata,
			);

			self.spawn_send_transaction(msg).await;
		}
	}

	/// Spawn a transaction task and try sending the transaction.
	pub async fn spawn_send_transaction(&self, msg: SolanaTxRequestMessage) {
		let task = SolTransactionTask::new(self.get_client());
		let payer = self.get_payer();

		if msg.is_bootstrap {
			task.try_send_solana_transaction(msg, &self.payer).await;
		} else {
			self.get_spawn_handle().spawn("send_solana_transaction", None, async move {
				task.try_send_solana_transaction(msg, &payer).await // 클로징된 payer 사용
			});
		}
	}
}

/// The transaction task for unsigned transactions.
pub struct SolTransactionTask {
	/// The solana client.
	sol_client: Arc<RpcClient>,
}

impl SolTransactionTask {
	/// Build an unsigned transaction task instance.
	// pub fn new(url: String) -> Self {
	//     let sol_client = Arc::new(RpcClient::new_with_commitment(
	//         url,
	//         CommitmentConfig::processed(),
	//     ));
	//     Self { sol_client }
	// }

	pub fn new(sol_client: Arc<RpcClient>) -> Self {
		Self { sol_client }
	}

	/// possble change max retries
	pub fn get_transaction_config(&self) -> RpcSendTransactionConfig {
		let commitment_config = CommitmentConfig::confirmed();
		RpcSendTransactionConfig {
			skip_preflight: false,
			preflight_commitment: Some(commitment_config.commitment),
			max_retries: None,
			..RpcSendTransactionConfig::default()
		}
	}

	async fn try_send_solana_transaction(
		&self,
		mut msg: SolanaTxRequestMessage,
		signers: &Keypair,
	) {
		if msg.retries_remaining == 0 {
			return;
		}

		let blockhash = self
			.sol_client
			.get_latest_blockhash()
			.await
			.map_err(|err| format!("error: unable to get latest blockhash: {err}"))
			.unwrap();

		let tx_config = self.get_transaction_config();

		match msg.tx_request.try_sign(&[signers], blockhash) {
			Ok(_) => match self
				.sol_client
				.send_and_confirm_transaction_with_spinner_and_config(
					&msg.tx_request,
					CommitmentConfig::finalized(),
					tx_config,
				)
				.await
			{
				Ok(_) => {
					// check_signatures.insert(*signature);
					todo!()
				},
				Err(e) => match e.get_transaction_error() {
					Some(TransactionError::BlockhashNotFound) => {
						todo!()
					},
					Some(TransactionError::AlreadyProcessed) => {
						todo!()
					},
					Some(e) => {
						log::error!(
							"TransactionError sending signature: {} error: {:?} tx: {:?}",
							msg.tx_request.get_signature(),
							e,
							msg.tx_request
						);
					},
					None => {
						log::error!(
							"Unknown error sending transaction signature: {} error: {:?}",
							msg.tx_request.get_signature(),
							e
						);
					},
				},
				Err(error) => {
					todo!()
				},
			},
			Err(error) => {
				todo!()
			},
		}
	}
}
