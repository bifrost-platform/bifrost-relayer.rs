use std::{sync::Arc, time::Duration};

use alloy::{
	dyn_abi::SolType,
	network::{Network, TransactionBuilder},
	primitives::{B256, ChainId, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::Log,
	signers::Signature,
	sol_types::SolValue,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::cccp,
	contracts::socket::{
		Socket_Struct::{Poll_Submit, Signatures, Socket_Message},
		Variants,
	},
	eth::{BootstrapState, BuiltRelayTransaction, SocketEventStatus},
	tx::HookMetadata,
	utils::{recover_message, sub_display_format},
};
use eyre::Result;
use sc_service::SpawnTaskHandle;
use tokio::time::sleep;

use crate::eth::{SUB_LOG_TARGET, avoid_race_condition, send_transaction};

use super::EthClient;

#[async_trait::async_trait]
pub trait Handler {
	/// Starts the event handler and listens to every new consumed block.
	async fn run(&mut self) -> Result<()>;

	/// Decode and parse the event if the given log triggered an relay target event.
	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) -> Result<()>;

	/// Verifies whether the given transaction interacted with the target contract.
	fn is_target_contract(&self, log: &Log) -> bool;

	/// Verifies whether the given event topic matches the target event signature.
	fn is_target_event(&self, topic: Option<&B256>) -> bool;
}

#[async_trait::async_trait]
/// The client to interact with the `Socket` contract instance.
pub trait SocketRelayBuilder<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Get the `EthClient` of the implemented handler.
	fn get_client(&self) -> Arc<EthClient<F, P, N>>;

	/// Builds the `poll()` function call data.
	fn build_poll_request(&self, msg: Socket_Message, sigs: Signatures) -> N::TransactionRequest {
		self.get_client()
			.protocol_contracts
			.socket
			.poll(Poll_Submit { msg, sigs, option: U256::default() })
			.into_transaction_request()
	}

	/// Builds the `poll()` transaction request.
	///
	/// This method returns an `Option` in order to bypass unknown chain events.
	/// Possibly can happen when a new chain definition hasn't been added to the operator's config.
	async fn build_transaction(
		&self,
		_msg: Socket_Message,
		_is_inbound: bool,
		_relay_tx_chain_id: ChainId,
	) -> Result<Option<BuiltRelayTransaction<N>>> {
		Ok(None)
	}

	/// Build the signatures required to request an inbound `poll()` and returns a flag which represents
	/// whether the relay transaction should be processed to an external chain.
	async fn build_inbound_signatures(&self, _msg: Socket_Message) -> Result<(Signatures, bool)> {
		Ok((Signatures::default(), false))
	}

	/// Build the signatures required to request an outbound `poll()` and returns a flag which represents
	/// whether the relay transaction should be processed to an external chain.
	async fn build_outbound_signatures(&self, _msg: Socket_Message) -> Result<(Signatures, bool)> {
		Ok((Signatures::default(), false))
	}

	/// Signs the given socket message.
	async fn sign_socket_message(&self, msg: Socket_Message) -> Result<Signature> {
		let encoded_msg: Vec<u8> = msg.into();
		Ok(self.get_client().sign_message(&encoded_msg).await?)
	}

	/// Get the signatures of the given message.
	async fn get_sorted_signatures(&self, msg: Socket_Message) -> Result<Signatures> {
		let client = self.get_client();
		let signatures = client
			.protocol_contracts
			.socket
			.get_signatures(msg.req_id.clone(), msg.status)
			.call()
			.await?;

		let mut signature_vec = Vec::<Signature>::from(signatures);
		signature_vec.sort_by_key(|k| recover_message(*k, &msg.abi_encode()));

		Ok(Signatures::from(signature_vec))
	}
}

#[async_trait::async_trait]
pub trait BootstrapHandler {
	/// Get the chain id.
	fn get_chain_id(&self) -> ChainId;

	/// Fetch the shared bootstrap data.
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData>;

	/// Starts the bootstrap process.
	async fn bootstrap(&self) -> Result<()>;

	/// Fetch the historical events to bootstrap.
	async fn get_bootstrap_events(&self) -> Result<Vec<Log>>;

	/// Set the bootstrap state for a chain.
	async fn set_bootstrap_state(&self, state: BootstrapState) {
		let bootstrap_shared_data = self.bootstrap_shared_data();
		let mut bootstrap_states = bootstrap_shared_data.bootstrap_states.write().await;
		*bootstrap_states.get_mut(&self.get_chain_id()).unwrap() = state;
	}

	/// Verifies whether the given chain is before the given bootstrap state.
	async fn is_before_bootstrap_state(&self, state: BootstrapState) -> bool {
		*self
			.bootstrap_shared_data()
			.bootstrap_states
			.read()
			.await
			.get(&self.get_chain_id())
			.unwrap() < state
	}

	/// Waits for the given chain synced to the normal start state.
	async fn wait_for_bootstrap_state(&self, state: BootstrapState) -> Result<()> {
		loop {
			let current_state = {
				let shared_data = self.bootstrap_shared_data();
				let bootstrap_states = shared_data.bootstrap_states.read().await;
				*bootstrap_states.get(&self.get_chain_id()).unwrap()
			};

			if current_state == state {
				break;
			}
			sleep(Duration::from_millis(100)).await;
		}
		Ok(())
	}

	/// Waits for all chains to be bootstrapped.
	async fn wait_for_all_chains_bootstrapped(&self) -> Result<()> {
		loop {
			let all_bootstrapped = {
				let shared_data = self.bootstrap_shared_data();
				let bootstrap_states = shared_data.bootstrap_states.read().await;
				bootstrap_states.values().all(|state| *state == BootstrapState::NormalStart)
			};

			if all_bootstrapped {
				break;
			}
			sleep(Duration::from_millis(100)).await;
		}
		Ok(())
	}
}

#[async_trait::async_trait]
pub trait HookExecutor<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// Whether to enable debug mode.
	fn debug_mode(&self) -> bool;

	/// Get the handle to spawn tasks.
	fn handle(&self) -> SpawnTaskHandle;

	/// Get the `EthClient` of the implemented handler.
	fn get_client(&self) -> Arc<EthClient<F, P, N>>;

	/// Get the `EthClient` of the bifrost network.
	fn get_bifrost_client(&self) -> Arc<EthClient<F, P, N>>;

	/// Get the `EthClient` of the system chain by chain id.
	fn get_system_client(&self, chain_id: &ChainId) -> Option<Arc<EthClient<F, P, N>>>;

	/// Validates common hook prerequisites for both execute and rollback operations.
	///
	/// This private helper method performs the following checks:
	/// 1. **Status Check**: Verifies that the socket message status matches the expected status.
	/// 2. **Bitcoin Chain Check**: Skips hook processing for Bitcoin chains (not supported).
	/// 3. **Source Client Validation**: Ensures the source chain client is configured.
	/// 4. **Hooks Contract Validation**: Ensures the source chain has a hooks contract.
	/// 5. **Refund Address Validation**: Ensures `msg.params.refund` matches the source chain's hooks contract address.
	///    This prevents unauthorized contracts from triggering hooks.
	/// 6. **Variant Decoding**: Attempts to decode `msg.params.variants` into `Variants` struct.
	///
	/// **Note**: Idempotency checks are NOT performed here. They are handled by the calling methods
	/// (`should_execute_hook` and `should_rollback_hook`) to allow context-specific validation.
	///
	/// # Arguments
	/// * `msg` - The socket message to validate.
	/// * `expected_status` - The expected socket event status (Executed or Rollbacked).
	///
	/// # Returns
	/// * `Ok(Some(Variants))` - If all checks pass.
	/// * `Ok(None)` - If any check fails, indicating the operation should be skipped.
	/// * `Err(_)` - If an RPC error occurs during verification.
	async fn should_process_hook(
		&self,
		msg: &Socket_Message,
		expected_status: SocketEventStatus,
	) -> Result<Option<Variants>> {
		let status = SocketEventStatus::from(msg.status);

		// Check if status matches expected
		if status != expected_status {
			return Ok(None);
		}

		// We don't support hooks on Bitcoin
		let src_chain_id = Into::<u32>::into(msg.req_id.ChainIndex) as ChainId;
		let dst_chain_id = Into::<u32>::into(msg.ins_code.ChainIndex) as ChainId;
		if let Some(bitcoin_chain_id) = self.get_bifrost_client().get_bitcoin_chain_id() {
			if src_chain_id == bitcoin_chain_id || dst_chain_id == bitcoin_chain_id {
				return Ok(None);
			}
		}

		// Get source chain client and hooks contract address
		let src_client = match self.get_system_client(&src_chain_id) {
			Some(client) => client,
			None => {
				br_primitives::log_and_capture!(
					warn,
					&self.get_client().get_chain_name(),
					SUB_LOG_TARGET,
					self.get_client().address().await,
					"⚠️  Source chain client not found for chain id: {}. Skipping hook processing.",
					src_chain_id
				);
				return Ok(None);
			},
		};

		let hooks_address = match &src_client.protocol_contracts.hooks {
			Some(hooks) => hooks.address(),
			None => {
				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ⏭️  Skipping hook processing: source chain has no hooks contract",
					sub_display_format(SUB_LOG_TARGET)
				);
				return Ok(None);
			},
		};

		// Validate refund address matches source chain hooks contract address
		if msg.params.refund != *hooks_address {
			log::info!(
				target: &self.get_client().get_chain_name(),
				"-[{}] ⏭️  Skipping hook processing: refund address {:?} != hooks address {:?}",
				sub_display_format(SUB_LOG_TARGET),
				msg.params.refund,
				hooks_address
			);
			return Ok(None);
		}

		// Decode the variants field. The data contains encoded struct content
		// without the outer tuple wrapper. For ABI decoding to work properly,
		// we need to prepend an offset pointer (0x20) that indicates where
		// the struct data begins (at byte 32).
		let mut wrapped_data = Vec::with_capacity(32 + msg.params.variants.len());
		wrapped_data.extend_from_slice(&[0u8; 31]); // 31 zero bytes
		wrapped_data.push(0x20); // offset = 32 bytes
		wrapped_data.extend_from_slice(&msg.params.variants);
		match <Variants as SolType>::abi_decode(&wrapped_data) {
			Ok(variants) => {
				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ✅ Decoded variants: sender={:?}, receiver={:?}, refund={:?}, max_tx_fee={}, message_len={}",
					sub_display_format(SUB_LOG_TARGET),
					variants.sender,
					variants.receiver,
					variants.refund,
					variants.max_tx_fee,
					variants.message.len()
				);
				return Ok(Some(variants));
			},
			Err(e) => {
				log::warn!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ⚠️  Failed to decode variants, skipping hook processing: {}",
					sub_display_format(SUB_LOG_TARGET),
					e
				);
				return Ok(None);
			},
		}
	}

	/// Validates if hooks execute should be called for this socket message.
	///
	/// This method delegates to `should_process_hook()` for common validation checks,
	/// then performs execute-specific idempotency verification.
	///
	/// # Idempotency Check
	/// Verifies that the request has not already been processed on the destination chain
	/// by checking `Hooks.processedRequests(req_id)`.
	///
	/// # Returns
	/// * `Ok(Some(Variants))` - If all checks pass and the hook should be executed.
	/// * `Ok(None)` - If any check fails, indicating execution should be skipped.
	/// * `Err(_)` - If an RPC error occurs during verification.
	async fn should_execute_hook(&self, msg: &Socket_Message) -> Result<Option<Variants>> {
		let variants = self.should_process_hook(msg, SocketEventStatus::Executed).await?;

		// Early return if common validation failed
		let variants = match variants {
			Some(v) => v,
			None => return Ok(None),
		};

		// Idempotency check on destination chain
		if let Some(hooks) = &self.get_client().protocol_contracts.hooks {
			let is_processed = hooks.processedRequests(msg.req_id.clone().into()).call().await?;

			if !is_processed {
				return Ok(Some(variants));
			}
			return Ok(None); // Already processed
		}

		// No hooks contract - skip execution
		Ok(None)
	}

	/// Estimates the gas required for hook execution and calculates the fee in the bridged asset.
	///
	/// This method performs the following steps:
	/// 1. **Gas Estimation**: Estimates the gas limit for the `Hooks.execute()` transaction on the destination chain.
	/// 2. **Fee Calculation (DNC)**: Calculates the estimated fee in the Destination Native Currency (DNC).
	/// 3. **Price Fetching**: Fetches the current prices of the DNC and the bridged asset in USD using cached oracles.
	/// 4. **Fee Conversion**: Converts the estimated DNC fee into the bridged asset amount, handling decimal differences
	///    (e.g., 18 decimals for DNC vs 6 decimals for USDC).
	/// 5. **Validation**: Checks if the calculated fee exceeds the `max_tx_fee` authorized by the user, and if `max_tx_fee`
	///    exceeds the total bridged amount.
	///
	/// # Arguments
	/// * `msg` - The socket message containing the transfer details.
	/// * `max_tx_fee` - The maximum fee authorized by the user (in bridged asset units).
	/// * `tx_request` - The transaction request to be updated with gas limit.
	///
	/// # Returns
	/// * `Ok(())` - If gas estimation and fee calculation succeed.
	/// * `Err(_)` - If any step fails (e.g., oracle failure, fee exceeds limit).
	async fn estimate_hook_gas(
		&self,
		msg: &Socket_Message,
		max_tx_fee: U256,
		tx_request: &N::TransactionRequest,
		is_inbound: bool,
	) -> Result<(U256, u64)> {
		// Validate: maxTxFee must not exceed the bridged amount
		if max_tx_fee > msg.params.amount {
			return Err(eyre::eyre!(
				"max_tx_fee ({}) exceeds bridged amount ({})",
				max_tx_fee,
				msg.params.amount
			));
		}

		// Estimate gas and fee on destination chain
		avoid_race_condition().await;
		let gas = match self.get_client().estimate_gas(tx_request.clone()).await {
			Ok(gas) => gas,
			Err(e) => {
				// Check if error is a revert, either directly or wrapped in retry error
				let error_string = e.to_string();
				let is_revert = if let Some(error_resp) = e.as_error_resp() {
					error_resp.as_revert_data().is_some()
				} else {
					// Handle case where error is wrapped by retry layer
					// Pattern: "Max retries exceeded ... execution reverted"
					error_string.contains("execution reverted") || error_string.contains("revert")
				};

				if is_revert {
					// we don't propagate the error if a contract reverted the estimated gas request
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  Hook.execute() estimated gas reverted: {}",
						error_string
					);
					return Ok((U256::ZERO, 0));
				}
				return Err(e.into());
			},
		};
		let gas_price = self.get_client().get_gas_price().await?;
		// DNC: Destination Chain's Native Currency
		let estimated_fee_in_dnc = U256::from(gas).checked_mul(U256::from(gas_price)).ok_or(
			eyre::eyre!("Gas fee calculation overflow: gas={}, gas_price={}", gas, gas_price),
		)?;

		// Fetch destination chain native currency price oracle
		let dnc_oracle_address = self
			.get_bifrost_client()
			.get_oracle_address_by_chain(self.get_client().metadata.id)
			.await?;
		let dnc_oracle_decimals =
			self.get_bifrost_client().get_oracle_decimals(dnc_oracle_address).await?;
		let dnc_native_oracle = self.get_bifrost_client().get_oracle(dnc_oracle_address).await;
		// Get destination native currency price (keep oracle decimals for precision)
		let dnc_price_scaled = match dnc_native_oracle.latestRoundData().call().await {
			Ok(data) => U256::from(data.answer),
			Err(e) => {
				// Check if the error is a revert (on-chain oracle failure)
				// Revert means the oracle is stale or not working, skip execution
				let error_string = e.to_string();
				let is_revert = if let Some(_revert_data) = e.as_revert_data() {
					true
				} else {
					// Handle case where error is wrapped by retry layer
					error_string.contains("execution reverted") || error_string.contains("revert")
				};

				if is_revert {
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  Destination native oracle reverted (oracle: {:?}). Skipping hook execution. Error: {}",
						dnc_oracle_address,
						error_string
					);
					return Ok((U256::ZERO, 0));
				}
				// Not a revert - this is an RPC/network error, propagate it
				return Err(e.into());
			},
		};

		// Fetch bridged asset decimals
		let bridged_asset = self
			.get_client()
			.get_asset_address_by_index(msg.params.tokenIDX0, is_inbound)
			.await?;
		let bridged_asset_decimals = if bridged_asset == cccp::NATIVE_CURRENCY_ADDRESS {
			18 // Native currency has 18 decimals (e.g. BFC, BNB, ETH, etc.)
		} else {
			self.get_client().get_erc20_decimals(bridged_asset).await?
		};

		// Fetch bridged asset price oracle
		let bridged_asset_oracle_address = self
			.get_bifrost_client()
			.get_oracle_address_by_asset_index(msg.params.tokenIDX0)
			.await?;
		let bridged_asset_oracle_decimals = self
			.get_bifrost_client()
			.get_oracle_decimals(bridged_asset_oracle_address)
			.await?;
		let bridged_asset_oracle =
			self.get_bifrost_client().get_oracle(bridged_asset_oracle_address).await;
		// Get bridged asset price (keep oracle decimals for precision)
		let bridged_asset_price_scaled = match bridged_asset_oracle.latestRoundData().call().await {
			Ok(data) => U256::from(data.answer),
			Err(e) => {
				// Check if the error is a revert (on-chain oracle failure)
				let error_string = e.to_string();
				let is_revert = if let Some(_revert_data) = e.as_revert_data() {
					true
				} else {
					// Handle case where error is wrapped by retry layer
					error_string.contains("execution reverted") || error_string.contains("revert")
				};

				if is_revert {
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  Bridged asset oracle reverted (token: {:?}). Skipping hook execution. Error: {}",
						msg.params.tokenIDX0,
						error_string
					);
					return Ok((U256::ZERO, 0)); // Skip execution, don't submit transaction
				}
				// Not a revert - this is an RPC/network error, propagate it
				return Err(e.into());
			},
		};

		// Calculate fee in bridged asset with overflow-safe two-stage approach
		//
		// Problem: Direct calculation can overflow U256 with high-value assets (e.g., Bitcoin at $100k)
		// Solution: Calculate price ratio first, then apply to fee with controlled intermediate values
		//
		// Stage 1: Calculate price ratio with 18 decimals precision
		//   price_ratio = (dnc_price_scaled × 10^18) / bridged_price_scaled
		//   Peak value: ~10^41 (safe, well below U256 max of 10^77)
		//
		// Stage 2: Apply ratio to fee with decimal adjustments
		//   fee = (estimated_fee × price_ratio × 10^bridged_decimals) / (10^18 × 10^18)
		//   Peak value: ~10^56 (safe, 21 orders of magnitude below U256 max)
		//
		// This preserves precision while guaranteeing zero overflow risk even with:
		// - Bitcoin at $1M: 10^24 oracle price ✅
		// - 100 ETH gas cost: 10^20 wei ✅
		// - Any combination of realistic crypto prices ✅
		//
		// Oracle decimal handling:
		// - When oracle decimals match: ratio calculation naturally preserves precision
		// - When different: adjust with oracle decimal ratio in stage 2

		// Stage 1: Calculate price ratio (DNC price / bridged price) with 18 decimals precision
		let price_ratio_scaled = dnc_price_scaled
			.checked_mul(U256::from(10u128.pow(18)))
			.ok_or(eyre::eyre!("Price ratio scaling overflow"))?
			.checked_div(bridged_asset_price_scaled)
			.ok_or(eyre::eyre!("Price ratio calculation failed (division by zero?)"))?;

		// Stage 2: Apply price ratio to estimated fee with decimal adjustments
		let fee_in_bridged_asset = estimated_fee_in_dnc
			.checked_mul(price_ratio_scaled)
			.ok_or(eyre::eyre!("Fee × price ratio overflow"))?
			.checked_mul(U256::from(10u128.pow(bridged_asset_decimals as u32)))
			.ok_or(eyre::eyre!("Bridged asset decimal adjustment overflow"))?
			.checked_div(U256::from(10u128.pow(18)))
			.ok_or(eyre::eyre!("DNC decimal normalization failed"))?
			.checked_div(U256::from(10u128.pow(18)))
			.ok_or(eyre::eyre!("Price ratio precision removal failed"))?
			// Adjust for oracle decimal differences if they don't match
			.checked_mul(U256::from(10u128.pow(bridged_asset_oracle_decimals as u32)))
			.ok_or(eyre::eyre!("Oracle decimal adjustment overflow"))?
			.checked_div(U256::from(10u128.pow(dnc_oracle_decimals as u32)))
			.ok_or(eyre::eyre!("Oracle decimal normalization failed"))?;

		// Validate: fee_in_bridged_asset <= maxTxFee
		if fee_in_bridged_asset > max_tx_fee {
			return Err(eyre::eyre!(
				"Estimated fee ({}) exceeds max_tx_fee ({})",
				fee_in_bridged_asset,
				max_tx_fee
			));
		}

		// Calculate human-readable prices for logging (convert to f64 to avoid truncation)
		// Oracle prices should fit in u128 range (max realistic price: $1M × 10^18 < 2^128)
		let dnc_price_display = if let Ok(price_u128) = u128::try_from(dnc_price_scaled) {
			(price_u128 as f64) / 10f64.powi(dnc_oracle_decimals as i32)
		} else {
			// Fallback for extremely large values
			(dnc_price_scaled / U256::from(10u128.pow(dnc_oracle_decimals as u32))).to::<u128>()
				as f64
		};
		let bridged_price_display =
			if let Ok(price_u128) = u128::try_from(bridged_asset_price_scaled) {
				(price_u128 as f64) / 10f64.powi(bridged_asset_oracle_decimals as i32)
			} else {
				(bridged_asset_price_scaled
					/ U256::from(10u128.pow(bridged_asset_oracle_decimals as u32)))
				.to::<u128>() as f64
			};

		log::info!(
			target: &self.get_client().get_chain_name(),
			"-[{}] 💰 Hook fee estimate: {} (bridged asset wei) <= max_tx_fee: {}. dst_price: ${:.6}, bridged_price: ${:.6}",
			sub_display_format(SUB_LOG_TARGET),
			fee_in_bridged_asset,
			max_tx_fee,
			dnc_price_display,
			bridged_price_display
		);

		Ok((fee_in_bridged_asset, gas))
	}

	/// Validates if hooks rollback should be called for this socket message.
	///
	/// This method delegates to `should_process_hook()` for common validation checks,
	/// then performs rollback-specific idempotency verification.
	///
	/// # Idempotency Check
	/// Verifies that the request has not already been rolled back on the source chain
	/// by checking `Hooks.rolledbackRequests(req_id)`.
	///
	/// # Returns
	/// * `Ok(Some(Variants))` - If all checks pass and the rollback should be executed.
	/// * `Ok(None)` - If any check fails, indicating rollback should be skipped.
	/// * `Err(_)` - If an RPC error occurs during verification.
	async fn should_rollback_hook(&self, msg: &Socket_Message) -> Result<Option<Variants>> {
		let variants = self.should_process_hook(msg, SocketEventStatus::Rollbacked).await?;

		// Early return if common validation failed
		let variants = match variants {
			Some(v) => v,
			None => return Ok(None),
		};

		// Idempotency check on source chain
		if let Some(hooks) = &self.get_client().protocol_contracts.hooks {
			let is_rollbacked = hooks.rolledbackRequests(msg.req_id.clone().into()).call().await?;

			if !is_rollbacked {
				return Ok(Some(variants));
			}
			return Ok(None); // Already rolled back
		}

		// No hooks contract - skip rollback
		Ok(None)
	}

	/// Executes the rollback hook on the destination chain.
	///
	/// This method constructs and sends the `Hooks.rollback()` transaction.
	/// Unlike execute, rollback doesn't require fee estimation or oracle calls
	/// as it's a simpler state update operation.
	///
	/// # Arguments
	/// * `msg` - The socket message containing details of the failed cross-chain transfer.
	/// * `variants` - The decoded variants containing execution parameters (sender, receiver, max_tx_fee, message).
	///
	/// # Returns
	/// * `Ok(())` - If the transaction is successfully submitted or correctly skipped.
	/// * `Err(_)` - If an error occurs during transaction building.
	async fn rollback_hook(&self, msg: &Socket_Message, variants: Variants) -> Result<()> {
		match &self.get_client().protocol_contracts.hooks {
			Some(hooks) => {
				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] 🔄 Calling Hooks.rollback() on chain {} for sequence: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.get_client().metadata.id,
					msg.req_id.sequence
				);

				avoid_race_condition().await;

				// Build the Hooks.rollback() call
				let tx_request = hooks
					.rollback(msg.clone().into())
					.into_transaction_request()
					.with_from(self.get_client().address().await);

				let metadata = HookMetadata::new(
					variants.sender,
					variants.receiver,
					variants.max_tx_fee,
					None,
					variants.message,
				);

				send_transaction(
					self.get_client().clone(),
					tx_request,
					format!("{} (Hooks.rollback())", SUB_LOG_TARGET),
					Arc::new(metadata),
					self.debug_mode(),
					self.handle(),
				);

				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ✅ Hooks.rollback() transaction submitted for sequence: {}",
					sub_display_format(SUB_LOG_TARGET),
					msg.req_id.sequence
				);
			},
			None => {
				log::debug!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ⏭️  Skipping Hooks.rollback(): no hooks contract configured",
					sub_display_format(SUB_LOG_TARGET)
				);
			},
		}
		Ok(())
	}

	/// Executes the hook on the destination chain.
	///
	/// This method constructs and sends the `Hooks.execute()` transaction.
	/// It performs the following checks and operations:
	/// 1. **Context Check**: Ensures the `Hooks` contract is configured for the client.
	/// 2. **Transaction Building**: Creates the initial `Hooks.execute()` transaction request.
	/// 3. **Gas Filling**: Calls `fill_hook_gas()` to estimate gas and validate fees.
	///    If `fill_hook_gas` fails or determines execution should be skipped (e.g., oracle revert),
	///    the transaction is not sent.
	/// 4. **Submission**: Sends the transaction using `send_transaction()`.
	///
	/// # Arguments
	/// * `msg` - The socket message containing details of the cross-chain transfer.
	/// * `variants` - The decoded variants containing execution parameters (sender, receiver, max_tx_fee, message).
	///
	/// # Returns
	/// * `Ok(())` - If the transaction is successfully submitted or correctly skipped.
	/// * `Err(_)` - If an error occurs during transaction building or gas estimation.
	async fn execute_hook(
		&self,
		msg: &Socket_Message,
		variants: Variants,
		is_inbound: bool,
	) -> Result<()> {
		match &self.get_client().protocol_contracts.hooks {
			Some(hooks) => {
				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] 🪝 Calling Hooks.execute() on chain {} with txFee: {} for sequence: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.get_client().metadata.id,
					variants.max_tx_fee,
					msg.req_id.sequence
				);

				// Build the Hooks.execute() call for initial gas estimation
				let init_tx_request = hooks
					.execute_0(variants.max_tx_fee, msg.clone().into())
					.into_transaction_request()
					.with_from(self.get_client().address().await);

				let (fee_in_bridged_asset, gas) = self
					.estimate_hook_gas(&msg, variants.max_tx_fee, &init_tx_request, is_inbound)
					.await?;

				if fee_in_bridged_asset.is_zero() || gas == 0 {
					// Skip execution, don't submit transaction
					return Ok(());
				}

				// Build the Hooks.execute() call for actual transaction submission
				let tx_request = hooks
					.execute_0(fee_in_bridged_asset, msg.clone().into())
					.into_transaction_request()
					.with_from(self.get_client().address().await)
					.with_gas_limit(gas);

				let metadata = HookMetadata::new(
					variants.sender,
					variants.receiver,
					variants.max_tx_fee,
					Some(fee_in_bridged_asset),
					variants.message,
				);

				send_transaction(
					self.get_client().clone(),
					tx_request,
					format!("{} (Hooks.execute())", SUB_LOG_TARGET),
					Arc::new(metadata),
					self.debug_mode(),
					self.handle(),
				);

				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ✅ Hooks.execute() transaction submitted for sequence: {}",
					sub_display_format(SUB_LOG_TARGET),
					msg.req_id.sequence
				);
			},
			None => return Ok(()),
		}
		Ok(())
	}
}
