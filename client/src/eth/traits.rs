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
	contracts::{
		erc20::Erc20Contract,
		oracle::OracleContract,
		socket::{
			Socket_Struct::{Poll_Submit, Signatures, Socket_Message},
			Variants,
		},
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
	/// Requirements:
	/// 1. Status must be Executed
	/// 2. msg.params.refund must match source chain hooks contract address
	/// 3. msg.params.variants must be valid and decodable
	///
	/// Returns: Option<Variants> - Some(variants) if should execute, None otherwise
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
					"-[{}] ⏭️  Skipping hooks execute: refund address {:?} != hooks address {:?}",
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
						"-[{}] ⚠️  Failed to decode variants, skipping hooks execute: {}",
						sub_display_format(SUB_LOG_TARGET),
						e
					);
					return Ok(None);
				},
			}
		}

		Ok(None)
	}

	async fn fill_hook_gas(
		&self,
		msg: &Socket_Message,
		max_tx_fee: U256,
		tx_request: &mut N::TransactionRequest,
	) -> Result<()> {
		// Estimate gas and fee on destination chain
		let gas = self.get_client().estimate_gas(tx_request.clone()).await?;
		let gas_price = self.get_client().get_gas_price().await?;
		let estimated_fee_on_dst = U256::from(gas as u128 * gas_price);

		let relay_queue = self
			.get_bifrost_client()
			.protocol_contracts
			.relay_queue
			.as_ref()
			.unwrap()
			.clone();
		let vault = self.get_client().protocol_contracts.vault.clone();

		// Fetch destination chain native currency price oracle
		let dst_native_oracle_address = relay_queue
			.get_native_currency_oracle(self.get_client().metadata.id as u32)
			.call()
			.await?;
		let dst_native_oracle =
			OracleContract::new(dst_native_oracle_address, self.get_bifrost_client().clone());
		let dst_oracle_decimals = dst_native_oracle.decimals().call().await?;
		let dst_latest_round_data = dst_native_oracle.latestRoundData().call().await?;

		// Get destination native currency price per full unit in USD
		let dst_native_currency_price = U256::from(dst_latest_round_data.answer)
			.checked_div(U256::from(10u128.pow(dst_oracle_decimals as u32)))
			.ok_or(eyre::eyre!("Failed to calculate destination native currency price"))?;

		// Fetch bridged asset decimals
		let bridged_asset = vault.assets_config(msg.params.tokenIDX0).call().await?.target;
		let erc20 = Erc20Contract::new(bridged_asset, self.get_client().clone());
		let bridged_asset_decimals = erc20.decimals().call().await?;

		// Fetch bridged asset price oracle
		let bridged_asset_oracle = OracleContract::new(
			relay_queue.get_asset_oracle_by_hash(msg.params.tokenIDX0).call().await?,
			self.get_bifrost_client().clone(),
		);
		let bridged_oracle_decimals = bridged_asset_oracle.decimals().call().await?;
		let bridged_latest_round_data = bridged_asset_oracle.latestRoundData().call().await?;

		// Get bridged asset price per full unit in USD
		let bridged_asset_price_usd = U256::from(bridged_latest_round_data.answer)
			.checked_div(U256::from(10u128.pow(bridged_oracle_decimals as u32)))
			.ok_or(eyre::eyre!("Failed to calculate bridged asset price"))?;

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
		let fee_in_bridged_asset = estimated_fee_on_dst
			.checked_mul(dst_native_currency_price)
			.ok_or(eyre::eyre!("Fee multiplication overflow"))?
			.checked_mul(U256::from(10u128.pow(bridged_asset_decimals as u32)))
			.ok_or(eyre::eyre!("Decimal adjustment overflow"))?
			.checked_div(bridged_asset_price_usd)
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

		// Validate: maxTxFee must not exceed the bridged amount
		if max_tx_fee > msg.params.amount {
			return Err(eyre::eyre!(
				"max_tx_fee ({}) exceeds bridged amount ({})",
				max_tx_fee,
				msg.params.amount
			));
		}

		log::info!(
			target: &self.get_client().get_chain_name(),
			"-[{}] 💰 Hook fee estimate: {} (bridged asset wei) <= max_tx_fee: {}. dst_price: ${}, bridged_price: ${}",
			sub_display_format(SUB_LOG_TARGET),
			fee_in_bridged_asset,
			max_tx_fee,
			dst_native_currency_price,
			bridged_asset_price_usd
		);

		tx_request.set_gas_limit(gas);
		Ok(())
	}

	/// Execute the hook.
	async fn execute_hook(&self, msg: &Socket_Message, variants: Variants) -> Result<()> {
		match &self.get_client().protocol_contracts.hooks {
			Some(hooks) => {
				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] 🪝 Calling hooks execute on chain {} with txFee: {} for sequence: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.get_client().metadata.id,
					variants.max_tx_fee,
					msg.req_id.sequence
				);

				// Build the hooks execute call
				let mut tx_request = hooks
					.execute_0(variants.max_tx_fee, msg.clone().into())
					.into_transaction_request()
					.with_from(self.get_client().address().await);

				self.fill_hook_gas(&msg, variants.max_tx_fee, &mut tx_request).await?;

				// Send the transaction
				let metadata = HookMetadata::new(
					variants.sender,
					variants.receiver,
					variants.max_tx_fee,
					variants.message,
				);

				// TODO: skip gas estimation and filling gas limit (since we already filled it)
				send_transaction(
					self.get_client().clone(),
					tx_request,
					format!("{} (hooks-execute)", SUB_LOG_TARGET),
					Arc::new(metadata),
					self.debug_mode(),
					self.handle(),
				);

				log::info!(
					target: &self.get_client().get_chain_name(),
					"-[{}] ✅ Hooks execute transaction submitted for sequence: {}",
					sub_display_format(SUB_LOG_TARGET),
					msg.req_id.sequence
				);
			},
			None => return Ok(()),
		}
		Ok(())
	}
}
