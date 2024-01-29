#[cfg(feature = "v2")]
use br_primitives::eth::SocketVariants;

use br_primitives::{
	eth::{BuiltRelayTransaction, ChainID, RecoveredSignature},
	socket::{PollSubmit, Signatures, SocketMessage},
};
use ethers::{
	abi::Token,
	providers::JsonRpcClient,
	types::{Bytes, Log, Signature, H256, U256},
};
use std::{str::FromStr, sync::Arc};

pub use execution_filter::*;
pub use roundup_relay_handler::*;
pub use socket_relay_handler::*;

use super::EthClient;

mod execution_filter;
mod roundup_relay_handler;
mod socket_relay_handler;

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

	#[cfg(feature = "v2")]
	/// Decodes the raw variants in the socket message.
	fn decode_msg_variants(&self, _raw_variants: &Bytes) -> SocketVariants {
		SocketVariants::default()
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
