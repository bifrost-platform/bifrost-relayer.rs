use serde::Deserialize;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::pubkey::Pubkey;
use sp_core::H256;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{Barrier, Mutex, RwLock};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	eth::{BootstrapState, ChainID, GasCoefficient},
	tx::{SolanaTxRequestMessage, SolanaTxRequestSender},
	utils::sub_display_format,
};

use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{Address as EthAddress, Address, TransactionRequest},
};

use solana_sdk::{
	commitment_config::CommitmentConfig,
	message::Message,
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

use crate::solana::{
	event::{EventManager as SOLEventManager, EventType, SolanaMessage},
	LOG_TARGET,
};

const SUB_LOG_TARGET: &str = "inbound-handler";

// The max amount of compute units for a transaction.
// TODO: consider a more sane value and/or use IGP gas payments instead.
const PROCESS_COMPUTE_UNITS: u32 = 1_400_000;

pub struct SolanaInboundHandler {
	/// The solana client.
	pub sol_client: Arc<RpcClient>,
	/// Sender that sends messages to tx request channel (Bifrost network)
	tx_request_sender: Arc<SolanaTxRequestSender>,
	/// The receiver connected to the tx request channel.
	receiver: UnboundedReceiver<SolanaTxRequestMessage>,
	/// Event type which this handler should handle.
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	payer: Keypair,
	pub program_id: Pubkey,
}

impl SolanaInboundHandler {
	pub fn new(
		sol_client: Arc<RpcClient>,
		tx_request_sender: Arc<SolanaTxRequestSender>,
		receiver: UnboundedReceiver<SolanaTxRequestMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		payer: Keypair,
		contract_address: H256,
	) -> Self {
		let program_id = Pubkey::from(<[u8; 32]>::from(contract_address));

		Self {
			sol_client,
			tx_request_sender,
			receiver,
			target_event: EventType::Inbound,
			bootstrap_shared_data,
			payer,
			program_id,
		}
	}

	async fn build_transaction(
		&self,
		msg: SolanaMessage,
		user_bfc_address: Address,
	) -> Transaction {
		let mut instructions = Vec::with_capacity(2);
		// Set the compute unit limit.
		instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(PROCESS_COMPUTE_UNITS));

		let commitment = CommitmentConfig::processed();

		let assoc = spl_associated_token_account::get_associated_token_address(
			&receiver_pubkey,
			&mint_account_pubkey,
		);

		let txn = Transaction::new_signed_with_payer(
			&instructions,
			Some(&payer.pubkey()),
			&[payer],
			recent_blockhash,
		);

		txn
	}

	// Request send socket relay transaction to the target event channel.
	// async fn request_send_transaction(
	// 	&self,
	// 	chain_id: ChainID,
	// 	tx_request: TransactionRequest,
	// 	metadata: SolanaRelayMetadata,
	// 	sub_log_target: &str,

	// ) {
	// 	todo!()
	// }
}
