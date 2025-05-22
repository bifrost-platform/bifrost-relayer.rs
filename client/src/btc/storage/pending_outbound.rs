use alloy::sol_types::SolValue;
use br_primitives::{contracts::socket::Socket_Struct::Socket_Message, substrate::BoundedVec};
use miniscript::bitcoin::{Address, Amount, address::NetworkUnchecked};
use std::{
	collections::{BTreeMap, HashMap},
	sync::Arc,
};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct PendingOutboundValue {
	pub socket_messages: Vec<Socket_Message>,
	pub amount: Amount,
}

#[derive(Debug, Clone, Default)]
pub struct PendingOutboundPool(
	Arc<RwLock<BTreeMap<Address<NetworkUnchecked>, PendingOutboundValue>>>,
);

impl PendingOutboundPool {
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

	pub async fn clear(&self) {
		self.0.write().await.clear();
	}

	fn encode_socket_messages(&self, messages: Vec<Socket_Message>) -> Vec<Vec<u8>> {
		messages.iter().map(|msg| msg.abi_encode()).collect()
	}
}
