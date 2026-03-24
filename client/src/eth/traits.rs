use std::{sync::Arc, time::Duration};

use alloy::{
	dyn_abi::SolType,
	network::{Network, TransactionBuilder},
	primitives::{Address, B256, ChainId, I256, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::Log,
	signers::Signature,
	sol_types::SolValue,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{cccp, config::CHAINLINK_STALENESS_THRESHOLD_VOLATILE},
	contracts::{
		chainlink_aggregator::ChainlinkContract,
		oracle::{OracleManagerContract, OracleManagerInstance},
		socket::{
			Socket_Struct::{Poll_Submit, Signatures, Socket_Message},
			Variants,
		},
	},
	eth::{BootstrapState, BuiltRelayTransaction, SocketEventStatus},
	substrate::{
		CustomConfig,
		bifrost_runtime::{self, runtime_types::bp_oracle::OracleKey},
	},
	tx::HookMetadata,
	utils::{recover_message, sub_display_format},
};
use eyre::Result;
use sc_service::SpawnTaskHandle;
use subxt::{
	OnlineClient,
	utils::{H160 as SubH160, H256 as SubH256},
};
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

		let mut keyed = Vec::<Signature>::from(signatures)
			.into_iter()
			.map(|sig| recover_message(sig, &msg.abi_encode()).map(|addr| (addr, sig)))
			.collect::<Result<Vec<_>, _>>()?;
		keyed.sort_by_key(|(addr, _)| *addr);

		Ok(Signatures::from(keyed.into_iter().map(|(_, sig)| sig).collect::<Vec<_>>()))
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

	/// Get the substrate online client.
	fn get_sub_client(&self) -> OnlineClient<CustomConfig>;

	/// Get the `EthClient` of the implemented handler.
	fn get_client(&self) -> Arc<EthClient<F, P, N>>;

	/// Get the `EthClient` of the bifrost network.
	fn get_bifrost_client(&self) -> Arc<EthClient<F, P, N>>;

	/// Get the `EthClient` of the system chain by chain id.
	fn get_system_client(&self, chain_id: &ChainId) -> Option<Arc<EthClient<F, P, N>>>;

	/// Resolves the oracle manager contract instance from the socket contract.
	async fn resolve_oracle_manager(&self) -> Result<OracleManagerInstance<F, P, N>> {
		let orc_manager_address =
			self.get_bifrost_client().protocol_contracts.socket.orc_manager().call().await?;
		Ok(OracleManagerContract::new(orc_manager_address, self.get_bifrost_client().provider()))
	}

	/// Fetches the oracle ID for a chain's native currency from `OracleRegistry.Oracles`.
	async fn get_oracle_oid_for_chain(&self, chain_id: ChainId) -> Result<Option<B256>> {
		self.fetch_oracle_oid(OracleKey::NativeCurrency(chain_id as u64)).await
	}

	/// Fetches the oracle ID for an EVM asset address from `OracleRegistry.Oracles`.
	async fn get_oracle_oid_for_asset(&self, asset: Address) -> Result<Option<B256>> {
		self.fetch_oracle_oid(OracleKey::Asset(SubH160(asset.0.0))).await
	}

	/// Checks if the given token index is hookable.
	async fn is_hookable(&self, asset_index_hash: B256) -> Result<bool> {
		match self
			.get_sub_client()
			.storage()
			.at_latest()
			.await?
			.fetch(
				&bifrost_runtime::storage()
					.cccp_relay_queue()
					.asset_indexes_hook_state(SubH256(asset_index_hash.0)),
			)
			.await
		{
			Ok(Some(is_hookable)) => Ok(is_hookable),
			Ok(None) => Ok(false),
			Err(e) => Err(e.into()),
		}
	}

	/// Fetches an oracle ID from `OracleRegistry.Oracles` using the typed subxt storage API.
	async fn fetch_oracle_oid(&self, oracle_key: OracleKey) -> Result<Option<B256>> {
		let result = self
			.get_sub_client()
			.storage()
			.at_latest()
			.await?
			.fetch(&bifrost_runtime::storage().oracle_registry().oracles(oracle_key))
			.await?;
		Ok(result.map(|oid| B256::from_slice(&oid.0)))
	}

	/// Fetches the canonical asset address from `CccpRelayQueue.AssetIndexes`.
	///
	/// Returns the `AssetId` (H160) registered for the given CCCP asset index hash.
	/// - `0xffff...ffff` indicates a chain's native currency.
	/// - Any other address is the unified ERC20 contract address.
	async fn fetch_asset_id_from_relay_queue(
		&self,
		asset_index_hash: B256,
	) -> Result<Option<Address>> {
		let result = self
			.get_sub_client()
			.storage()
			.at_latest()
			.await?
			.fetch(
				&bifrost_runtime::storage()
					.cccp_relay_queue()
					.asset_indexes(SubH256(asset_index_hash.0)),
			)
			.await?;
		Ok(result.map(|h160| Address::from_slice(&h160.0)))
	}

	/// Fetches the Chainlink aggregator address and decimal precision for the given asset
	/// from `OracleRegistry.Aggregators`.
	///
	/// Returns `(aggregator_address, decimal)` if found, or `None` if no aggregator is registered.
	async fn fetch_aggregator_info(&self, asset_id: Address) -> Result<Option<(Address, u8)>> {
		let result = self
			.get_sub_client()
			.storage()
			.at_latest()
			.await?
			.fetch(&bifrost_runtime::storage().oracle_registry().aggregators(SubH160(asset_id.0.0)))
			.await?;
		Ok(result.map(|info| (Address::from_slice(&info.address.0), info.decimal)))
	}

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
				log::warn!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ⏭️  Skipping hook processing: source chain has no hooks contract",
					sub_display_format(SUB_LOG_TARGET)
				);
				return Ok(None);
			},
		};

		// Validate refund address matches source chain hooks contract address
		if msg.params.refund != *hooks_address {
			log::warn!(
				target: &self.get_client().get_chain_name(),
				"-[{}] ⏭️  Skipping hook processing: refund address {:?} != hooks address {:?}",
				sub_display_format(SUB_LOG_TARGET),
				msg.params.refund,
				hooks_address
			);
			return Ok(None);
		}

		// Check if msg.params.tokenIDX0 is hookable
		if !self.is_hookable(msg.params.tokenIDX0).await? {
			return Ok(None);
		}

		// Skip decoding when variants payload is empty (e.g. no hook data).
		// Decoding empty data would cause Overrun since Variants has multiple fields.
		if msg.params.variants.is_empty() {
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

		Ok(None) // No hooks contract - skip execution
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
					error_string.contains("execution reverted") || error_string.contains("revert")
				};

				if is_revert {
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

		// Resolve oracle manager from the socket contract
		let oracle_manager = self.resolve_oracle_manager().await?;

		// Fetch DNC oracle ID: destination chain native currency → OracleKey::NativeCurrency(dst_chain_id)
		let dst_chain_id = self.get_client().metadata.id;
		let dnc_oid = match self.get_oracle_oid_for_chain(dst_chain_id).await? {
			Some(oid) => oid,
			None => {
				br_primitives::log_and_capture!(
					warn,
					&self.get_client().get_chain_name(),
					SUB_LOG_TARGET,
					self.get_client().address().await,
					"⚠️  DNC oracle ID not found for chain {}. Skipping hook execution.",
					dst_chain_id
				);
				return Ok((U256::ZERO, 0));
			},
		};

		// Resolve the unified asset address for the bridged token from the relay queue pallet.
		// AssetIndexes maps tokenIDX0 → AssetId (H160) registered in the oracle registry.
		let bridged_asset_id =
			match self.fetch_asset_id_from_relay_queue(msg.params.tokenIDX0).await? {
				Some(id) => id,
				None => {
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  Bridged asset not found in relay queue for tokenIDX0 {:?}. Skipping hook execution.",
						msg.params.tokenIDX0
					);
					return Ok((U256::ZERO, 0));
				},
			};

		// Fetch bridged asset decimals from the destination chain
		let bridged_asset = self
			.get_client()
			.get_asset_address_by_index(msg.params.tokenIDX0, is_inbound)
			.await?;
		let bridged_asset_decimals = if bridged_asset == cccp::NATIVE_CURRENCY_ADDRESS {
			18 // Native currency has 18 decimals (e.g. BFC, BNB, ETH, etc.)
		} else {
			self.get_client().get_erc20_decimals(bridged_asset).await?
		};

		// Bridged asset oracle ID: always look up by the unified AssetId from the relay queue.
		let bridged_oid = match self.get_oracle_oid_for_asset(bridged_asset_id).await? {
			Some(oid) => oid,
			None => {
				br_primitives::log_and_capture!(
					warn,
					&self.get_client().get_chain_name(),
					SUB_LOG_TARGET,
					self.get_client().address().await,
					"⚠️  Bridged asset oracle ID not found for {:?}. Skipping hook execution.",
					bridged_asset_id
				);
				return Ok((U256::ZERO, 0));
			},
		};

		let now = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap_or_default()
			.as_secs();

		// Fetch DNC price from oracle manager
		let dnc_price = match oracle_manager.last_oracle_info(dnc_oid).call().await {
			Ok(info) => {
				let staleness = now.saturating_sub(info._1.time);
				if staleness > CHAINLINK_STALENESS_THRESHOLD_VOLATILE {
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  DNC oracle price is stale (last updated {}s ago, threshold: {}s). Skipping hook execution.",
						staleness,
						CHAINLINK_STALENESS_THRESHOLD_VOLATILE
					);
					return Ok((U256::ZERO, 0));
				}
				let price = U256::from_be_bytes(info._1.data.into());
				if price.is_zero() {
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  DNC oracle returned zero price. Skipping hook execution."
					);
					return Ok((U256::ZERO, 0));
				}
				price
			},
			Err(e) => {
				br_primitives::log_and_capture!(
					warn,
					&self.get_client().get_chain_name(),
					SUB_LOG_TARGET,
					self.get_client().address().await,
					"⚠️  DNC oracle price fetch failed (chain: {}). Skipping hook execution. Error: {}",
					dst_chain_id,
					e.to_string()
				);
				return Ok((U256::ZERO, 0));
			},
		};

		// Fetch bridged asset price: use aggregator contract if oracle ID is zero,
		// otherwise fetch from the oracle manager.
		let bridged_price = if bridged_oid == B256::ZERO {
			// A zero oracle ID signals that pricing comes from an aggregator contract.
			let (aggregator_address, aggregator_decimals) =
				match self.fetch_aggregator_info(bridged_asset_id).await? {
					Some(info) => info,
					None => {
						br_primitives::log_and_capture!(
							warn,
							&self.get_client().get_chain_name(),
							SUB_LOG_TARGET,
							self.get_client().address().await,
							"⚠️  Oracle aggregator not found for asset {:?}. Skipping hook execution.",
							bridged_asset_id
						);
						return Ok((U256::ZERO, 0));
					},
				};

			let aggregator =
				ChainlinkContract::new(aggregator_address, self.get_bifrost_client().provider());
			match aggregator.latestRoundData().call().await {
				Ok(round_data) => {
					let staleness =
						now.saturating_sub(round_data.updatedAt.try_into().unwrap_or_default());
					if staleness > CHAINLINK_STALENESS_THRESHOLD_VOLATILE {
						br_primitives::log_and_capture!(
							warn,
							&self.get_client().get_chain_name(),
							SUB_LOG_TARGET,
							self.get_client().address().await,
							"⚠️  Oracle aggregator price is stale (last updated {}s ago, threshold: {}s) for {:?}. Skipping hook execution.",
							staleness,
							CHAINLINK_STALENESS_THRESHOLD_VOLATILE,
							bridged_asset_id
						);
						return Ok((U256::ZERO, 0));
					}

					let answer = round_data.answer;
					if answer <= I256::ZERO {
						br_primitives::log_and_capture!(
							warn,
							&self.get_client().get_chain_name(),
							SUB_LOG_TARGET,
							self.get_client().address().await,
							"⚠️  Oracle aggregator returned non-positive price for {:?}. Skipping hook execution.",
							bridged_asset_id
						);
						return Ok((U256::ZERO, 0));
					}
					// Normalize to 18-decimal scale to match the oracle manager's price format.
					let price_raw = answer.into_raw();
					let normalized = if aggregator_decimals <= 18 {
						price_raw
							.checked_mul(
								U256::from(10u64).pow(U256::from(18u8 - aggregator_decimals)),
							)
							.ok_or(eyre::eyre!("Oracle aggregator price normalization overflow"))?
					} else {
						price_raw
							.checked_div(
								U256::from(10u64).pow(U256::from(aggregator_decimals - 18u8)),
							)
							.ok_or(eyre::eyre!("Oracle aggregator price normalization underflow"))?
					};
					if normalized.is_zero() {
						br_primitives::log_and_capture!(
							warn,
							&self.get_client().get_chain_name(),
							SUB_LOG_TARGET,
							self.get_client().address().await,
							"⚠️  Oracle aggregator normalized price is zero for {:?}. Skipping hook execution.",
							bridged_asset_id
						);
						return Ok((U256::ZERO, 0));
					}
					normalized
				},
				Err(e) => {
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  Oracle aggregator price fetch failed (token: {:?}). Skipping hook execution. Error: {}",
						bridged_asset_id,
						e.to_string()
					);
					return Ok((U256::ZERO, 0));
				},
			}
		} else {
			// Fetch bridged asset price from oracle manager
			match oracle_manager.last_oracle_info(bridged_oid).call().await {
				Ok(info) => {
					let staleness = now.saturating_sub(info._1.time);
					if staleness > CHAINLINK_STALENESS_THRESHOLD_VOLATILE {
						br_primitives::log_and_capture!(
							warn,
							&self.get_client().get_chain_name(),
							SUB_LOG_TARGET,
							self.get_client().address().await,
							"⚠️  Bridged asset oracle price is stale (last updated {}s ago, threshold: {}s). Skipping hook execution.",
							staleness,
							CHAINLINK_STALENESS_THRESHOLD_VOLATILE
						);
						return Ok((U256::ZERO, 0));
					}
					let price = U256::from_be_bytes(info._1.data.into());
					if price.is_zero() {
						br_primitives::log_and_capture!(
							warn,
							&self.get_client().get_chain_name(),
							SUB_LOG_TARGET,
							self.get_client().address().await,
							"⚠️  Bridged asset oracle returned zero price. Skipping hook execution."
						);
						return Ok((U256::ZERO, 0));
					}
					price
				},
				Err(e) => {
					br_primitives::log_and_capture!(
						warn,
						&self.get_client().get_chain_name(),
						SUB_LOG_TARGET,
						self.get_client().address().await,
						"⚠️  Bridged asset oracle price fetch failed (token: {:?}). Skipping hook execution. Error: {}",
						bridged_asset,
						e.to_string()
					);
					return Ok((U256::ZERO, 0));
				},
			}
		};

		// Fee conversion: same-scale oracle prices, no decimal correction needed.
		//
		// fee_bridged = estimated_fee_in_dnc × dnc_price × 10^bridged_decimals
		//               ─────────────────────────────────────────────────────────
		//                            bridged_price × 10^18
		//
		// Both prices come from the same OracleManager (unified scale), so no
		// oracle-decimal adjustment is required.
		let fee_in_bridged_asset = estimated_fee_in_dnc
			.checked_mul(dnc_price)
			.ok_or(eyre::eyre!("Fee × DNC price overflow"))?
			.checked_mul(U256::from(10u64).pow(U256::from(bridged_asset_decimals)))
			.ok_or(eyre::eyre!("Bridged asset decimal adjustment overflow"))?
			.checked_div(bridged_price)
			.ok_or(eyre::eyre!("Division by bridged price failed (zero?)"))?
			.checked_div(U256::from(10u64).pow(U256::from(18u8)))
			.ok_or(eyre::eyre!("DNC decimal normalization failed"))?;

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
			"-[{}] 💰 Hook fee estimate: {} (bridged asset wei) <= max_tx_fee: {}. dnc_price: {}, bridged_price: {}",
			sub_display_format(SUB_LOG_TARGET),
			fee_in_bridged_asset,
			max_tx_fee,
			dnc_price,
			bridged_price,
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
