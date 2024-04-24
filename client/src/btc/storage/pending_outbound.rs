use br_primitives::contracts::socket::SocketMessage;
use miniscript::bitcoin::{address::NetworkUnchecked, Address, Amount};
use std::collections::HashMap;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct PendingOutboundValue {
	pub socket_message: SocketMessage,
	pub amount: Amount,
}

#[derive(Debug, Clone)]
pub struct PendingOutboundPool {
	inner: Arc<RwLock<BTreeMap<Address<NetworkUnchecked>, Vec<PendingOutboundValue>>>>,
}

impl PendingOutboundPool {
	pub fn new() -> Self {
		Self { inner: Default::default() }
	}

	pub async fn insert(
		&self,
		key: Address<NetworkUnchecked>,
		value: PendingOutboundValue,
	) -> Option<Vec<PendingOutboundValue>> {
		let mut write_lock = self.inner.write().await;
		return match write_lock.get_mut(&key) {
			Some(t) => {
				t.push(value);
				Some(t.clone())
			},
			None => write_lock.insert(key, vec![value]),
		};
	}

	pub async fn remove(
		&self,
		key: &Address<NetworkUnchecked>,
		amount: Amount,
	) -> Option<Vec<PendingOutboundValue>> {
		let mut write_lock = self.inner.write().await;
		return match write_lock.get_mut(key) {
			Some(t) => {
				for i in 0..t.len() {
					if t[i].amount == amount {
						t.remove(i);
						return Some(t.clone());
					}
				}
				None
			},
			None => None,
		};
	}

	pub async fn get(
		&self,
		key: &Address<NetworkUnchecked>,
		amount: Amount,
	) -> Option<PendingOutboundValue> {
		let read_lock = self.inner.read().await;
		match read_lock.get(key) {
			Some(t) => {
				for item in t {
					if item.amount == amount {
						return Some(item.clone());
					}
				}
				None
			},
			None => None,
		}
	}

	pub async fn pop_next_outputs(&self) -> HashMap<String, Amount> {
		let mut ret = HashMap::new();
		let mut keys_to_remove = vec![];

		let mut write_lock = self.inner.write().await;
		for (address, amount_vec) in write_lock.iter_mut() {
			if let Some(first_amount) = amount_vec.pop() {
				ret.insert(address.assume_checked_ref().to_string(), first_amount.amount);
				if amount_vec.is_empty() {
					keys_to_remove.push(address.clone());
				}
			}
		}

		for key in keys_to_remove {
			write_lock.remove(&key);
		}

		ret
	}
}

impl From<BTreeMap<Address<NetworkUnchecked>, Vec<PendingOutboundValue>>> for PendingOutboundPool {
	fn from(value: BTreeMap<Address<NetworkUnchecked>, Vec<PendingOutboundValue>>) -> Self {
		Self { inner: Arc::new(RwLock::new(value)) }
	}
}

impl From<&[(Address<NetworkUnchecked>, Vec<PendingOutboundValue>)]> for PendingOutboundPool {
	fn from(value: &[(Address<NetworkUnchecked>, Vec<PendingOutboundValue>)]) -> Self {
		Self { inner: Arc::new(RwLock::new(value.iter().cloned().collect())) }
	}
}
