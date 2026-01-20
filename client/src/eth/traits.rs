use std::{sync::Arc, time::Duration};

use alloy::{
	dyn_abi::DynSolValue,
	network::{Network, TransactionBuilder},
	primitives::{B256, ChainId, FixedBytes, U256},
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

use crate::eth::{SUB_LOG_TARGET, send_transaction};

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

	fn encode_socket_message(&self, msg: Socket_Message) -> Vec<u8> {
		let req_id = DynSolValue::Tuple(vec![
			DynSolValue::FixedBytes(
				FixedBytes::<32>::right_padding_from(msg.req_id.ChainIndex.as_slice()),
				4,
			),
			DynSolValue::Uint(U256::from(msg.req_id.round_id), 64),
			DynSolValue::Uint(U256::from(msg.req_id.sequence), 128),
		]);
		let status = DynSolValue::Uint(U256::from(msg.status), 8);
		let ins_code = DynSolValue::Tuple(vec![
			DynSolValue::FixedBytes(
				FixedBytes::<32>::right_padding_from(msg.ins_code.ChainIndex.as_slice()),
				4,
			),
			DynSolValue::FixedBytes(
				FixedBytes::<32>::right_padding_from(msg.ins_code.RBCmethod.as_slice()),
				16,
			),
		]);
		let params = DynSolValue::Tuple(vec![
			DynSolValue::FixedBytes(msg.params.tokenIDX0, 32),
			DynSolValue::FixedBytes(msg.params.tokenIDX1, 32),
			DynSolValue::Address(msg.params.refund),
			DynSolValue::Address(msg.params.to),
			DynSolValue::Uint(msg.params.amount, 256),
			DynSolValue::Bytes(msg.params.variants.to_vec()),
		]);

		DynSolValue::Tuple(vec![req_id, status, ins_code, params]).abi_encode()
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
		Ok(self.get_client().sign_message(&self.encode_socket_message(msg)).await?)
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

	/// Validates if hooks execute should be called for this socket message.
	///
	/// This method performs several critical checks to ensure the safety and validity of the hook execution:
	/// 1. **Status Check**: Verifies that the socket message status is `Executed`.
	/// 2. **Refund Address Validation**: Ensures `msg.params.refund` matches the source chain's hooks contract address.
	///    This prevents unauthorized contracts from triggering hooks.
	/// 3. **Variant Decoding**: Attempts to decode `msg.params.variants` into `Variants` struct.
	/// 4. **Idempotency Check**: Verifies that the request has not already been processed or rollbacked on the destination chain.
	///
	/// # Returns
	/// * `Ok(Some(Variants))` - If all checks pass and the hook should be executed.
	/// * `Ok(None)` - If any check fails (e.g., status mismatch, invalid refund address), indicating execution should be skipped.
	/// * `Err(_)` - If an RPC error occurs during verification.
	async fn should_execute_hook(&self, msg: &Socket_Message) -> Result<Option<Variants>> {
		let status = SocketEventStatus::from(msg.status);

		// Requirement 1: Check if status is Executed
		if status != SocketEventStatus::Executed {
			return Ok(None);
		}

		// Get source chain client and hooks contract address
		let src_chain_id = Into::<u32>::into(msg.req_id.ChainIndex) as ChainId;
		if let Some(src_client) = self.get_system_client(&src_chain_id) {
			let hooks_address = match &src_client.protocol_contracts.hooks {
				Some(hooks) => hooks.address(),
				None => return Ok(None),
			};

			// Requirement 2: msg.params.refund must match source chain hooks contract address
			if msg.params.refund != *hooks_address {
				log::debug!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ⏭️  Skipping Hooks.execute(): refund address {:?} != hooks address {:?}",
					sub_display_format(SUB_LOG_TARGET),
					msg.params.refund,
					hooks_address
				);
				return Ok(None);
			}

			// Requirement 3: Decode variants and verify if the request is not processed or rollbacked
			match Variants::abi_decode(&msg.params.variants) {
				Ok(variants) => {
					log::debug!(
						target: &self.get_client().get_chain_name(),
						"-[{}] ✅ Decoded variants: sender={:?}, receiver={:?}, max_tx_fee={}, message_len={}",
						sub_display_format(SUB_LOG_TARGET),
						variants.sender,
						variants.receiver,
						variants.max_tx_fee,
						variants.message.len()
					);

					if let Some(dst_hooks) = self.get_client().protocol_contracts.hooks.clone() {
						let is_processed =
							dst_hooks.processedRequests(msg.req_id.clone().into()).call().await?;
						let is_rollbacked =
							dst_hooks.rolledbackRequests(msg.req_id.clone().into()).call().await?;

						if !is_processed && !is_rollbacked {
							return Ok(Some(variants));
						}
					}
				},
				Err(e) => {
					log::warn!(
						target: &self.get_client().get_chain_name(),
						"-[{}] ⚠️  Failed to decode variants, skipping Hooks.execute(): {}",
						sub_display_format(SUB_LOG_TARGET),
						e
					);
					return Ok(None);
				},
			}
		}

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
	async fn fill_hook_gas(
		&self,
		msg: &Socket_Message,
		max_tx_fee: U256,
		tx_request: &mut N::TransactionRequest,
	) -> Result<()> {
		// Validate: maxTxFee must not exceed the bridged amount
		if max_tx_fee > msg.params.amount {
			return Err(eyre::eyre!(
				"max_tx_fee ({}) exceeds bridged amount ({})",
				max_tx_fee,
				msg.params.amount
			));
		}

		// Estimate gas and fee on destination chain
		let gas = self.get_client().estimate_gas(tx_request.clone()).await?;
		let gas_price = self.get_client().get_gas_price().await?;
		// DNC: Destination Chain's Native Currency
		let estimated_fee_in_dnc = U256::from(gas as u128 * gas_price);

		// Fetch destination chain native currency price oracle
		let dnc_oracle_address = self
			.get_bifrost_client()
			.get_oracle_address_by_chain(self.get_client().metadata.id)
			.await?;
		let dnc_oracle_decimals =
			self.get_bifrost_client().get_oracle_decimals(dnc_oracle_address).await?;
		let dnc_native_oracle = self.get_bifrost_client().get_oracle(dnc_oracle_address).await;
		// Get destination native currency price per full unit in USD
		let dnc_price_in_usd = match dnc_native_oracle.latestRoundData().call().await {
			Ok(data) => U256::from(data.answer)
				.checked_div(U256::from(10u128.pow(dnc_oracle_decimals as u32)))
				.ok_or(eyre::eyre!("Failed to calculate destination native currency price"))?,
			Err(e) => {
				// Check if the error is a revert (on-chain oracle failure)
				// Revert means the oracle is stale or not working, skip execution
				if let Some(_revert_data) = e.as_revert_data() {
					log::warn!(
						target: &self.get_client().get_chain_name(),
						"-[{}] ⚠️  Destination native oracle reverted (oracle: {:?}). Skipping hook execution.",
						sub_display_format(SUB_LOG_TARGET),
						dnc_oracle_address
					);
					return Ok(());
				}
				// Not a revert - this is an RPC/network error, propagate it
				log::error!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ❌ RPC error fetching destination native oracle price (oracle: {:?}): {}",
					sub_display_format(SUB_LOG_TARGET),
					dnc_oracle_address,
					e
				);
				return Err(e.into());
			},
		};

		// Fetch bridged asset decimals
		let bridged_asset =
			self.get_client().get_asset_address_by_index(msg.params.tokenIDX0).await?;
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
		// Get bridged asset price per full unit in USD
		let bridged_asset_price_in_usd = match bridged_asset_oracle.latestRoundData().call().await {
			Ok(data) => U256::from(data.answer)
				.checked_div(U256::from(10u128.pow(bridged_asset_oracle_decimals as u32)))
				.ok_or(eyre::eyre!("Failed to calculate bridged asset price"))?,
			Err(e) => {
				// Check if the error is a revert (on-chain oracle failure)
				if let Some(_revert_data) = e.as_revert_data() {
					log::warn!(
						target: &self.get_client().get_chain_name(),
						"-[{}] ⚠️  Bridged asset oracle reverted (token: {:?}). Skipping hook execution.",
						sub_display_format(SUB_LOG_TARGET),
						msg.params.tokenIDX0
					);
					return Ok(()); // Skip execution, don't submit transaction
				}
				// Not a revert - this is an RPC/network error, propagate it
				log::error!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ❌ RPC error fetching bridged asset oracle price (token: {:?}): {}",
					sub_display_format(SUB_LOG_TARGET),
					msg.params.tokenIDX0,
					e
				);
				return Err(e.into());
			},
		};

		// Calculate fee in bridged asset with proper decimal handling
		//
		// Conversion formula:
		// fee_in_bridged_asset = (estimated_fee_wei × dst_price_usd × 10^bridged_decimals) /
		//                        (bridged_asset_price_usd × 10^18)
		//
		// How It Works:
		// 1. estimated_fee_on_dst: Gas fee in wei (18 decimals) on destination chain
		// 2. Multiply by dst_native_currency_price: Convert to USD value
		// 3. Multiply by 10^bridged_decimals: Scale to bridged asset's decimal places
		// 4. Divide by bridged_asset_price_usd: Convert USD to bridged asset units
		// 5. Divide by 10^18: Remove the destination chain's 18 decimal normalization
		//
		// Example Scenarios:
		// - USDC (6 decimals): If fee is 0.001 ETH ($3), it correctly converts to ~3,000,000 USDC units (6 decimals)
		// - WBTC (8 decimals): If fee is 0.001 ETH ($3), it correctly converts to ~0.00005000 BTC units (8 decimals)
		// - Standard ERC20 (18 decimals): Works as before
		let fee_in_bridged_asset = estimated_fee_in_dnc
			.checked_mul(dnc_price_in_usd)
			.ok_or(eyre::eyre!("Fee multiplication overflow"))?
			.checked_mul(U256::from(10u128.pow(bridged_asset_decimals as u32)))
			.ok_or(eyre::eyre!("Decimal adjustment overflow"))?
			.checked_div(bridged_asset_price_in_usd)
			.ok_or(eyre::eyre!("Price division failed"))?
			.checked_div(U256::from(10u128.pow(18)))
			.ok_or(eyre::eyre!("Failed to convert fee to bridged asset units"))?;

		// Validate: fee_in_bridged_asset <= maxTxFee
		if fee_in_bridged_asset > max_tx_fee {
			return Err(eyre::eyre!(
				"Estimated fee ({}) exceeds max_tx_fee ({})",
				fee_in_bridged_asset,
				max_tx_fee
			));
		}

		log::info!(
			target: &self.get_client().get_chain_name(),
			"-[{}] 💰 Hook fee estimate: {} (bridged asset wei) <= max_tx_fee: {}. dst_price: ${}, bridged_price: ${}",
			sub_display_format(SUB_LOG_TARGET),
			fee_in_bridged_asset,
			max_tx_fee,
			dnc_price_in_usd,
			bridged_asset_price_in_usd
		);

		tx_request.set_gas_limit(gas);
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
	async fn execute_hook(&self, msg: &Socket_Message, variants: Variants) -> Result<()> {
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

				// Build the Hooks.execute() call
				let mut tx_request = hooks
					.execute_0(variants.max_tx_fee, msg.clone().into())
					.into_transaction_request()
					.with_from(self.get_client().address().await);

				self.fill_hook_gas(&msg, variants.max_tx_fee, &mut tx_request).await?;
				if tx_request.gas_limit().is_none() {
					// Skip execution, don't submit transaction
					return Ok(());
				}

				let metadata = HookMetadata::new(
					variants.sender,
					variants.receiver,
					variants.max_tx_fee,
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
