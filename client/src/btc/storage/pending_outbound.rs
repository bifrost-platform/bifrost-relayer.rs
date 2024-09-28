use br_primitives::contracts::socket::SocketMessage;
use br_primitives::substrate::BoundedVec;
use ethers::abi::Token;
use miniscript::bitcoin::{address::NetworkUnchecked, Address, Amount};
use std::{
	collections::{BTreeMap, HashMap},
	sync::Arc,
};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct PendingOutboundValue {
	pub socket_messages: Vec<SocketMessage>,
	pub amount: Amount,
}

#[derive(Debug, Clone)]
pub struct PendingOutboundPool(
	Arc<RwLock<BTreeMap<Address<NetworkUnchecked>, PendingOutboundValue>>>,
);

impl PendingOutboundPool {
	pub fn new() -> Self {
		Self(Default::default())
	}

	pub async fn insert(
		&self,
		key: Address<NetworkUnchecked>,
		value: PendingOutboundValue,
	) -> Option<PendingOutboundValue> {
		let mut write_lock = self.0.write().await;
		match write_lock.get_mut(&key) {
			Some(t) => {
				// If the socket message is already in the list, we don't want to add it again
				if t.socket_messages.iter().any(|x| value.socket_messages.contains(x)) {
					return None;
				}

				t.amount += value.amount;
				for x in value.socket_messages {
					t.socket_messages.push(x);
				}

				Some(t.clone())
			},
			None => write_lock.insert(key, value),
		}
	}

	pub async fn get(&self, key: &Address<NetworkUnchecked>) -> Option<PendingOutboundValue> {
		self.0.read().await.get(key).cloned()
	}

	pub async fn pop_next_outputs(
		&self,
	) -> (HashMap<String, Amount>, Vec<(BoundedVec<u8>, Vec<Vec<u8>>)>) {
		let mut outputs = HashMap::new();
		let mut socket_messages = vec![];
		let mut keys_to_remove = vec![];

		let mut write_lock = self.0.write().await;
		for (address, value) in write_lock.iter_mut() {
			outputs.insert(address.assume_checked_ref().to_string(), value.amount);
			socket_messages.push((
				BoundedVec(address.assume_checked_ref().to_string().into_bytes()),
				self.encode_socket_messages(value.socket_messages.clone()),
			));

			keys_to_remove.push(address.clone());
		}

		for key in keys_to_remove {
			write_lock.remove(&key);
		}

		(outputs, socket_messages)
	}

	fn encode_socket_messages(&self, messages: Vec<SocketMessage>) -> Vec<Vec<u8>> {
		messages
			.into_iter()
			.map(|msg| {
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
			})
			.collect()
	}
}
