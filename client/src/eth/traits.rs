use std::{sync::Arc, time::Duration};

use alloy::{
	dyn_abi::DynSolValue,
	network::AnyNetwork,
	primitives::{B256, ChainId, FixedBytes, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::{Log, TransactionInput},
	signers::Signature,
	sol_types::SolValue,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::socket::Socket_Struct::{Poll_Submit, Signatures, Socket_Message},
	eth::{BootstrapState, BuiltRelayTransaction},
	utils::recover_message,
};
use eyre::Result;
use tokio::time::sleep;

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
pub trait SocketRelayBuilder<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// Get the `EthClient` of the implemented handler.
	fn get_client(&self) -> Arc<EthClient<F, P>>;

	/// Builds the `poll()` function call data.
	fn build_poll_call_data(&self, msg: Socket_Message, sigs: Signatures) -> TransactionInput {
		let poll_submit = Poll_Submit { msg, sigs, option: U256::default() };
		TransactionInput::new(
			self.get_client().protocol_contracts.socket.poll(poll_submit).calldata().clone(),
		)
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
	) -> Result<Option<BuiltRelayTransaction>> {
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
