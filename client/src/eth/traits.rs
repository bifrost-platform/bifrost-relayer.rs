use std::{error::Error, str::FromStr, sync::Arc, time::Duration};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::socket::{PollSubmit, Signatures, SocketMessage},
	eth::{BootstrapState, BuiltRelayTransaction, ChainID, GasCoefficient, RecoveredSignature},
	tx::{FlushMetadata, TxRequest, TxRequestMessage, TxRequestMetadata},
	utils::sub_display_format,
};
use ethers::{
	abi::Token,
	providers::JsonRpcClient,
	types::{Bytes, Log, Signature, Transaction, TransactionReceipt, TxHash, H256, U256},
};
use sc_service::SpawnTaskHandle;
use tokio::time::sleep;

use super::EthClient;

#[async_trait::async_trait]
pub trait Handler {
	/// Starts the event handler and listens to every new consumed block.
	async fn run(&mut self);

	/// Decode and parse the event if the given log triggered an relay target event.
	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool);

	/// Verifies whether the given transaction interacted with the target contract.
	fn is_target_contract(&self, log: &Log) -> bool;

	/// Verifies whether the given event topic matches the target event signature.
	fn is_target_event(&self, topic: H256) -> bool;
}

#[async_trait::async_trait]
/// The client to interact with the `Socket` contract instance.
pub trait SocketRelayBuilder<T> {
	/// Get the `EthClient` of the implemented handler.
	fn get_client(&self) -> Arc<EthClient<T>>;

	/// Builds the `poll()` function call data.
	fn build_poll_call_data(&self, msg: SocketMessage, sigs: Signatures) -> Bytes
	where
		T: JsonRpcClient,
	{
		let poll_submit = PollSubmit { msg, sigs, option: U256::default() };
		self.get_client()
			.protocol_contracts
			.socket
			.poll(poll_submit)
			.calldata()
			.unwrap()
	}

	/// Builds the `poll()` transaction request.
	///
	/// This method returns an `Option` in order to bypass unknown chain events.
	/// Possibly can happen when a new chain definition hasn't been added to the operator's config.
	async fn build_transaction(
		&self,
		_msg: SocketMessage,
		_is_inbound: bool,
		_relay_tx_chain_id: ChainID,
	) -> Option<BuiltRelayTransaction> {
		None
	}

	/// Build the signatures required to request an inbound `poll()` and returns a flag which represents
	/// whether the relay transaction should be processed to an external chain.
	async fn build_inbound_signatures(&self, _msg: SocketMessage) -> (Signatures, bool) {
		(Signatures::default(), false)
	}

	/// Build the signatures required to request an outbound `poll()` and returns a flag which represents
	/// whether the relay transaction should be processed to an external chain.
	async fn build_outbound_signatures(&self, _msg: SocketMessage) -> (Signatures, bool) {
		(Signatures::default(), false)
	}

	/// Encodes the given socket message to bytes.
	fn encode_socket_message(&self, msg: SocketMessage) -> Vec<u8> {
		let req_id_token = Token::Tuple(vec![
			Token::FixedBytes(msg.req_id.chain.into()),
			Token::Uint(msg.req_id.round_id.into()),
			Token::Uint(msg.req_id.sequence.into()),
		]);
		let status_token = Token::Uint(msg.status.into());
		let ins_code_token = Token::Tuple(vec![
			Token::FixedBytes(msg.ins_code.chain.into()),
			Token::FixedBytes(msg.ins_code.method.into()),
		]);
		let params_token = Token::Tuple(vec![
			Token::FixedBytes(msg.params.token_idx0.into()),
			Token::FixedBytes(msg.params.token_idx1.into()),
			Token::Address(msg.params.refund),
			Token::Address(msg.params.to),
			Token::Uint(msg.params.amount),
			Token::Bytes(msg.params.variants.to_vec()),
		]);

		ethers::abi::encode(&[Token::Tuple(vec![
			req_id_token,
			status_token,
			ins_code_token,
			params_token,
		])])
	}

	/// Signs the given socket message.
	fn sign_socket_message(&self, msg: SocketMessage) -> Signature {
		let encoded_msg = self.encode_socket_message(msg);
		self.get_client().wallet.sign_message(&encoded_msg)
	}

	/// Get the signatures of the given message.
	async fn get_sorted_signatures(&self, msg: SocketMessage) -> Signatures
	where
		T: JsonRpcClient,
	{
		let raw_sigs = self
			.get_client()
			.contract_call(
				self.get_client()
					.protocol_contracts
					.socket
					.get_signatures(msg.clone().req_id, msg.clone().status),
				"socket.get_signatures",
			)
			.await;

		let raw_concated_v = &raw_sigs.v.to_string()[2..];

		let mut recovered_sigs = vec![];
		let encoded_msg = self.encode_socket_message(msg);
		for idx in 0..raw_sigs.r.len() {
			let sig = Signature {
				r: raw_sigs.r[idx].into(),
				s: raw_sigs.s[idx].into(),
				v: u64::from_str_radix(&raw_concated_v[idx * 2..idx * 2 + 2], 16).unwrap(),
			};
			recovered_sigs.push(RecoveredSignature::new(
				idx,
				sig,
				self.get_client().wallet.recover_message(sig, &encoded_msg),
			));
		}
		recovered_sigs.sort_by_key(|k| k.signer);

		let mut sorted_sigs = Signatures::default();
		let mut sorted_concated_v = String::from("0x");
		recovered_sigs.into_iter().for_each(|sig| {
			let idx = sig.idx;
			sorted_sigs.r.push(raw_sigs.r[idx]);
			sorted_sigs.s.push(raw_sigs.s[idx]);
			let v = Bytes::from([sig.signature.v as u8]);
			sorted_concated_v.push_str(&v.to_string()[2..]);
		});
		sorted_sigs.v = Bytes::from_str(&sorted_concated_v).unwrap();

		sorted_sigs
	}
}

#[async_trait::async_trait]
pub trait LegacyGasMiddleware {
	/// Get the current gas price of the network.
	async fn get_gas_price(&self) -> U256;

	/// Get gas_price for legacy retry transaction request.
	///
	/// Returns `max(current_network_gas_price,escalated_gas_price)`
	async fn get_gas_price_for_retry(
		&self,
		previous_gas_price: U256,
		gas_price_coefficient: f64,
		min_gas_price: U256,
	) -> U256;

	/// Get gas_price for escalated legacy transaction request. This will be only used when
	/// `is_initially_escalated` is enabled.
	async fn get_gas_price_for_escalation(
		&self,
		gas_price: U256,
		gas_price_coefficient: f64,
		min_gas_price: U256,
	) -> U256;

	/// Handles the failed gas price rpc request.
	async fn handle_failed_get_gas_price(&self, retries_remaining: u8, error: String) -> U256;
}

#[async_trait::async_trait]
pub trait Eip1559GasMiddleware {
	/// Gets a heuristic recommendation of max fee per gas and max priority fee per gas for EIP-1559
	/// compatible transactions.
	async fn get_estimated_eip1559_fees(&self) -> (U256, U256);

	/// Handles the failed eip1559 fees rpc request.
	async fn handle_failed_get_estimated_eip1559_fees(
		&self,
		retries_remaining: u8,
		error: String,
	) -> (U256, U256);
}

#[async_trait::async_trait]
/// The manager trait for Legacy and Eip1559 transactions.
pub trait TransactionManager<T>
where
	T: JsonRpcClient,
{
	/// Starts the transaction manager. Listens to every new consumed tx request message.
	async fn run(&mut self);

	/// Initialize transaction manager.
	async fn initialize(&mut self);

	/// Get the `EthClient`.
	fn get_client(&self) -> Arc<EthClient<T>>;

	/// Get the transaction spawn handle.
	fn get_spawn_handle(&self) -> SpawnTaskHandle;

	/// Spawn a transaction task and try sending the transaction.
	async fn spawn_send_transaction(&self, msg: TxRequestMessage);

	/// The flag whether the client has enabled txpool namespace.
	fn is_txpool_enabled(&self) -> bool;

	/// Flush all transaction from mempool.
	async fn flush_stuck_transaction(&self) {
		if self.is_txpool_enabled() && !self.get_client().metadata.is_native {
			let mempool = self.get_client().get_txpool_content().await;
			br_metrics::increase_rpc_calls(&self.get_client().get_chain_name());

			let mut transactions = Vec::new();
			transactions.extend(
				mempool.queued.get(&self.get_client().address()).cloned().unwrap_or_default(),
			);
			transactions.extend(
				mempool.pending.get(&self.get_client().address()).cloned().unwrap_or_default(),
			);

			for (_nonce, transaction) in transactions {
				self.spawn_send_transaction(TxRequestMessage::new(
					self.stuck_transaction_to_transaction_request(&transaction).await,
					TxRequestMetadata::Flush(FlushMetadata::default()),
					false,
					false,
					GasCoefficient::Low,
					true,
				))
				.await;
			}
		}
	}

	/// Converts stuck transaction to `TxRequest(TransactionRequest | Eip1559TransactionRequest)`
	async fn stuck_transaction_to_transaction_request(
		&self,
		transaction: &Transaction,
	) -> TxRequest;
}

#[async_trait::async_trait]
pub trait TransactionTask<T>
where
	T: JsonRpcClient,
{
	/// The flag whether the client has enabled txpool namespace.
	fn is_txpool_enabled(&self) -> bool;

	/// Get the `EthClient`.
	fn get_client(&self) -> Arc<EthClient<T>>;

	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	fn duplicate_confirm_delay(&self) -> Duration;

	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged such as
	/// gas estimation failures.
	fn debug_mode(&self) -> bool;

	/// Verifies whether the relayer has sufficient funds to pay for the transaction.
	async fn is_sufficient_funds(&self, gas_price: U256, gas: U256) -> bool {
		let fee = gas_price.saturating_mul(gas);
		let client = self.get_client();
		let balance = client.get_balance(client.address()).await;
		br_metrics::increase_rpc_calls(&client.get_chain_name());
		if balance < fee {
			return false;
		}
		true
	}

	/// Function that query mempool for check if the relay event that this relayer is about to send
	/// has already been processed by another relayer.
	async fn is_duplicate_relay(&self, tx_request: &TxRequest, check_mempool: bool) -> bool {
		let client = self.get_client();

		// does not check the txpool if the following condition satisfies
		// 1. the txpool namespace is disabled for the client
		// 2. the txpool check flag is false
		// 3. the client is Bifrost (native)
		if !self.is_txpool_enabled() || !check_mempool || client.metadata.is_native {
			return false;
		}

		let (data, to, from) = (
			tx_request.get_data(),
			tx_request.get_to().as_address().unwrap(),
			tx_request.get_from(),
		);

		let mempool = client.get_txpool_content().await;
		br_metrics::increase_rpc_calls(&client.get_chain_name());

		for (_address, tx_map) in mempool.pending.iter().chain(mempool.queued.iter()) {
			for (_nonce, mempool_tx) in tx_map.iter() {
				if mempool_tx.to.unwrap_or_default() == *to && mempool_tx.input == *data {
					// Trying gas escalating is not duplicate action
					if mempool_tx.from == *from {
						return false;
					}

					sleep(self.duplicate_confirm_delay()).await;
					return if client.get_transaction_receipt(mempool_tx.hash).await.is_some() {
						// if others relay processed
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						true
					} else {
						// if others relay stalled in mempool
						br_metrics::increase_rpc_calls(&client.get_chain_name());
						false
					};
				}
			}
		}
		false
	}

	/// Retry send_transaction() for failed transaction execution.
	async fn retry_transaction(&self, mut msg: TxRequestMessage, escalation: bool) {
		if !escalation {
			msg.build_retry_event();
			sleep(Duration::from_millis(msg.retry_interval)).await;
		}
		self.try_send_transaction(msg).await;
	}

	/// Sends the consumed transaction request to the connected chain. The transaction send will
	/// be retry if the transaction fails to be mined in a block.
	async fn try_send_transaction(&self, msg: TxRequestMessage);

	/// Handles the successful transaction receipt.
	fn handle_success_tx_receipt(
		&self,
		sub_target: &str,
		receipt: TransactionReceipt,
		metadata: TxRequestMetadata,
	) {
		let client = self.get_client();

		let status = receipt.status.unwrap();
		log::info!(
			target: &client.get_chain_name(),
			"-[{}] 🎁 The requested transaction has been successfully mined in block: {}, {:?}-{:?}-{:?}",
			sub_display_format(sub_target),
			metadata.to_string(),
			receipt.block_number.unwrap(),
			status,
			receipt.transaction_hash
		);
		if status.is_zero() && self.debug_mode() {
			let log_msg = format!(
				"-[{}]-[{}] ⚠️  Warning! Error encountered during contract execution [execution reverted]. A prior transaction might have been already submitted: {}, {:?}-{:?}-{:?}",
				sub_display_format(sub_target),
				client.address(),
				metadata,
				receipt.block_number.unwrap(),
				status,
				receipt.transaction_hash
			);
			log::warn!(target: &client.get_chain_name(), "{log_msg}");
			sentry::capture_message(
				&format!("[{}]{log_msg}", &client.get_chain_name()),
				sentry::Level::Warning,
			);
		}
		br_metrics::set_payed_fees(&client.get_chain_name(), &receipt);
	}

	/// Handles the stalled transaction.
	async fn handle_stalled_tx(
		&self,
		sub_target: &str,
		msg: TxRequestMessage,
		pending: TxHash,
		escalation: bool,
	) {
		let client = self.get_client();

		let log_msg = format!(
			"-[{}]-[{}] ♻️ The pending transaction has been stalled over 3 blocks. Try gas-escalation: {}-{}",
			sub_display_format(sub_target),
			client.address(),
			msg.metadata,
			pending
		);
		log::warn!(target: &client.get_chain_name(), "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", &client.get_chain_name()),
			sentry::Level::Warning,
		);

		self.retry_transaction(msg, escalation).await;
	}

	/// Handles the failed transaction receipt generation.
	async fn handle_failed_tx_receipt(&self, sub_target: &str, msg: TxRequestMessage) {
		let client = self.get_client();

		let log_msg = format!(
			"-[{}]-[{}] ♻️  The requested transaction failed to generate a receipt: {}, Retries left: {:?}",
			sub_display_format(sub_target),
			client.address(),
			msg.metadata,
			msg.retries_remaining - 1
		);
		log::error!(target: &client.get_chain_name(), "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", &client.get_chain_name()),
			sentry::Level::Error,
		);

		self.retry_transaction(msg, false).await;
	}

	/// Handles the failed transaction request.
	async fn handle_failed_tx_request<E: Error + Sync + ?Sized>(
		&self,
		sub_target: &str,
		msg: TxRequestMessage,
		error: &E,
	) {
		let client = self.get_client();

		let log_msg = format!(
			"-[{}]-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(sub_target),
			client.address(),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		log::error!(target: &client.get_chain_name(), "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", &client.get_chain_name()),
			sentry::Level::Error,
		);

		self.retry_transaction(msg, false).await;
	}

	/// Handles the failed gas estimation.
	async fn handle_failed_gas_estimation<E: Error + Sync + ?Sized>(
		&self,
		sub_target: &str,
		msg: TxRequestMessage,
		error: &E,
	) {
		let client = self.get_client();

		if self.debug_mode() {
			let log_msg = format!(
				"-[{}]-[{}] ⚠️  Warning! Error encountered during gas estimation: {}, Retries left: {:?}, Error: {}",
				sub_display_format(sub_target),
				client.address(),
				msg.metadata,
				msg.retries_remaining - 1,
				error.to_string()
			);
			log::warn!(target: &client.get_chain_name(), "{log_msg}");
			sentry::capture_message(
				&format!("[{}]{log_msg}", &client.get_chain_name()),
				sentry::Level::Warning,
			);
		}
		self.retry_transaction(msg, false).await;
	}
}

#[async_trait::async_trait]
pub trait BootstrapHandler {
	/// Fetch the shared bootstrap data.
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData>;

	/// Starts the bootstrap process.
	async fn bootstrap(&self);

	/// Fetch the historical events to bootstrap.
	async fn get_bootstrap_events(&self) -> Vec<Log>;

	/// Verifies whether the bootstrap state has been synced to the given state.
	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data()
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}
