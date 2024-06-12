use br_primitives::contracts::socket::SocketMessage;
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
pub struct PendingOutboundPool {
	inner: Arc<RwLock<BTreeMap<Address<NetworkUnchecked>, PendingOutboundValue>>>,
}

impl PendingOutboundPool {
	pub fn new() -> Self {
		Self { inner: Default::default() }
	}

	pub async fn insert(
		&self,
		key: Address<NetworkUnchecked>,
		value: PendingOutboundValue,
	) -> Option<PendingOutboundValue> {
		let mut write_lock = self.inner.write().await;
		return match write_lock.get_mut(&key) {
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
		};
	}

	pub async fn get(&self, key: &Address<NetworkUnchecked>) -> Option<PendingOutboundValue> {
		self.inner.read().await.get(key).cloned()
	}

	pub async fn pop_next_outputs(
		&self,
	) -> (HashMap<String, Amount>, BTreeMap<Vec<u8>, Vec<SocketMessage>>) {
		let mut outputs = HashMap::new();
		let mut socket_messages = BTreeMap::new();
		let mut keys_to_remove = vec![];

		let mut write_lock = self.inner.write().await;
		for (address, value) in write_lock.iter_mut() {
			outputs.insert(address.assume_checked_ref().to_string(), value.amount);
			socket_messages.insert(
				address.assume_checked_ref().to_string().into_bytes(),
				value.socket_messages.clone(),
			);

			keys_to_remove.push(address.clone());
		}

		for key in keys_to_remove {
			write_lock.remove(&key);
		}

		(outputs, socket_messages)
	}
}
