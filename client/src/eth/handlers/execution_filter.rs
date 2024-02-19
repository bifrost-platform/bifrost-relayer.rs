use br_primitives::{eth::GasCoefficient, sub_display_format};
use ethers::{
	providers::{JsonRpcClient, RawCall},
	types::{
		spoof::{self, State},
		Bytes, TransactionRequest, H160, H256,
	},
};
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::time::sleep;

use crate::eth::{
	EthClient, EventMessage, EventMetadata, EventSender, SocketRelayMetadata, TxRequest,
	DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS,
};

use super::SocketRelayBuilder;

const SUB_LOG_TARGET: &str = "execution-filter";

/// The slot index for token balances.
const SLOT_INDEX: u32 = 6;

/// The coin address used for bridge relay.
const COIN_ASSET_ADDRESS: &str = "0xffffffffffffffffffffffffffffffffffffffff";

/// The call result of the general message.
enum CallResult {
	/// The general message is executable. This contains the result in bytes.
	Success(Bytes),
	/// The general message is non-executable. This contains the error message.
	Revert(String),
}

/// The task that tries to pre-ethCall the general message.
/// This only handles requests that are relayed to the target client.
/// (`client` and `event_sender` are connected to the same chain)
pub struct ExecutionFilter<T> {
	/// The `EthClient` to interact with the connected blockchain.
	client: Arc<EthClient<T>>,
	/// The event senders that sends messages to the event channel.
	event_sender: Arc<EventSender>,
	/// The metadata of the given socket message.
	metadata: SocketRelayMetadata,
}

impl<T: JsonRpcClient> ExecutionFilter<T> {
	/// Instantiates a new `ExecutionFilter`.
	pub fn new(
		client: Arc<EthClient<T>>,
		event_sender: Arc<EventSender>,
		metadata: SocketRelayMetadata,
	) -> Self {
		Self { client, event_sender, metadata }
	}

	/// Return the remote (destination chain's) token index by the source chain's token index.
	async fn get_remote_token_idx(&self, src_token_idx: [u8; 32]) -> [u8; 32] {
		self.client
			.contract_call(
				self.client.protocol_contracts.vault.remote_asset_pair(src_token_idx),
				"vault.remote_asset_pair",
			)
			.await
	}

	/// Builds the `safe_execute()` transaction request.
	async fn build_transaction(&mut self) -> Option<TransactionRequest> {
		if let Some(executor) = &self.client.protocol_contracts.executor {
			// set the proper destination chain's token index
			self.metadata.token_idx0 = self.get_remote_token_idx(self.metadata.token_idx0).await;
			let call_data = executor
				.safe_execute(
					self.metadata.receiver,                   // params.to
					self.metadata.amount,                     // params.amount
					self.metadata.src_chain_id.to_be_bytes(), // req_id.chain
					self.metadata.variants.sender,            // variants.sender
					self.metadata.variants.message.clone(),   // variants.message
					self.metadata.token_idx0,                 // get_remote_asset_pair(params.token_idx0)
					self.metadata.token_idx1,                 // params.token_idx1
				)
				.calldata()
				.unwrap();

			return Some(
				TransactionRequest::default()
					.data(call_data)
					.to(executor.address())
					.from(executor.address()),
			);
		}
		None
	}

	/// Builds the state override key.
	fn build_override_key(&self) -> H256 {
		let executor_address = &self.client.protocol_contracts.executor.clone().unwrap().address();
		let mut account_hash = [0u8; 32];
		account_hash[32 - executor_address.0.len()..].copy_from_slice(&executor_address.0);

		let slot_index = Bytes::from(u32::to_be_bytes(SLOT_INDEX));
		let mut slot_index_hash = [0u8; 32];
		slot_index_hash[32 - slot_index.0.len()..].copy_from_slice(&slot_index.0);

		// returns the keccak256 hash of the concatenated account and slot index hashes
		// this is the key used to override the contract storage
		H256::from(ethers::utils::keccak256([account_hash, slot_index_hash].concat()))
	}

	/// Builds the state override object.
	async fn build_override_state(&self) -> State {
		let asset_address = match self.metadata.is_inbound {
			true => {
				// if inbound, fetch the asset address from the vault contract
				self.client
					.contract_call(
						self.client
							.protocol_contracts
							.vault
							.assets_config(self.metadata.token_idx0),
						"vault.assets_config",
					)
					.await
					.4
			},
			false => {
				// if outbound, fetch the asset address by slicing the token index
				H160::from_slice(&self.metadata.token_idx0[12..])
			},
		};
		// check if the asset is coin or token
		if asset_address == H160::from_str(COIN_ASSET_ADDRESS).unwrap() {
			spoof::balance(
				self.client.protocol_contracts.executor.clone().unwrap().address(),
				self.metadata.amount,
			)
		} else {
			spoof::storage(
				asset_address,
				self.build_override_key(),
				H256::from_low_u64_be(self.metadata.amount.as_u64()),
			)
		}
	}

	/// Tries to pre-ethCall the general message and on success cases,
	/// sends a transaction request to the event channel.
	pub async fn try_filter_and_send(
		&mut self,
		tx_request: TransactionRequest,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		if let Some(transaction) = self.build_transaction().await {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⛓️  Start Execution::Filter: {}",
				sub_display_format(SUB_LOG_TARGET),
				self.metadata
			);

			let result = match self.try_state_override_call(TxRequest::Legacy(transaction)).await {
				CallResult::Success(result) => result,
				CallResult::Revert(error) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] ⛓️  Execution::Filter Result → ❗️ Failure::Error({}): {}",
						sub_display_format(SUB_LOG_TARGET),
						error,
						self.metadata
					);
					return;
				},
			};

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⛓️  Execution::Filter Result → ✅ Success::CallResult({}): {}",
				sub_display_format(SUB_LOG_TARGET),
				result,
				self.metadata
			);

			self.request_send_transaction(tx_request, give_random_delay, gas_coefficient);
		}
	}

	/// Requests a new transaction request to the event channel.
	fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		match self.event_sender.send(EventMessage::new(
			TxRequest::Legacy(tx_request),
			EventMetadata::SocketRelay(self.metadata.clone()),
			true,
			give_random_delay,
			gas_coefficient,
			self.metadata.is_bootstrap,
		)) {
			Ok(()) => {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔖 Request relay transaction to chain({:?}): {}",
					sub_display_format(SUB_LOG_TARGET),
					&self.client.get_chain_id(),
					self.metadata
				);
			},
			Err(error) => {
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to send relay transaction to chain({:?}): {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					&self.client.get_chain_id(),
					self.metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to send relay transaction to chain({:?}): {}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						&self.client.get_chain_id(),
						self.metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	/// Tries to call the given general message with overrided state.
	async fn try_state_override_call(&self, general_msg: TxRequest) -> CallResult {
		match self
			.client
			.provider
			.call_raw(&general_msg.to_typed())
			.state(&self.build_override_state().await)
			.await
		{
			Ok(result) => {
				// this request is executable. now try to send the relay transaction.
				CallResult::Success(result)
			},
			Err(error) => {
				// this request is not executable. first, it'll be retried several times.
				// if it still remains as failure, this request will be ignored and post-handled by the `SocketRollbackHandler`.
				self.handle_failed_call(&general_msg, error.to_string()).await
			},
		}
	}

	/// Handles the failed eth call.
	async fn handle_failed_call(&self, tx_request: &TxRequest, error: String) -> CallResult {
		let mut retries = DEFAULT_CALL_RETRIES;
		let mut last_error = error;

		while retries > 0 {
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			if self.client.debug_mode {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during state override call, Retries left: {:?}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					retries - 1,
					last_error
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during state override call, Retries left: {:?}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						retries - 1,
						last_error
					)
					.as_str(),
					sentry::Level::Warning,
				);
			}

			match self.client.provider.call_raw(&tx_request.to_typed()).await {
				Ok(result) => return CallResult::Success(result),
				Err(error) => {
					sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					retries -= 1;
					last_error = error.to_string();
				},
			}
		}
		CallResult::Revert(last_error)
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> SocketRelayBuilder<T> for ExecutionFilter<T> {
	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}
}
