use miniscript::bitcoin::address::NetworkUnchecked;
use miniscript::bitcoin::{Address, Amount};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PendingOutboundState {
	Accepted,
	Executed,
	Committed, // remove PendingOutboundValue from pool when state reach here
}

#[derive(Debug, Clone)]
pub struct PendingOutboundValue {
	pub amount: Amount,
	pub state: PendingOutboundState,
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
		write_lock.insert(key, value)
	}

	pub async fn remove(&self, key: &Address<NetworkUnchecked>) -> Option<PendingOutboundValue> {
		let mut write_lock = self.inner.write().await;
		write_lock.remove(key)
	}

	pub async fn get(&self, key: &Address<NetworkUnchecked>) -> Option<PendingOutboundValue> {
		let read_lock = self.inner.read().await;
		read_lock.get(key).cloned()
	}
}

impl From<BTreeMap<Address<NetworkUnchecked>, PendingOutboundValue>> for PendingOutboundPool {
	fn from(value: BTreeMap<Address<NetworkUnchecked>, PendingOutboundValue>) -> Self {
		Self { inner: Arc::new(RwLock::new(value)) }
	}
}

impl From<&[(Address<NetworkUnchecked>, PendingOutboundValue)]> for PendingOutboundPool {
	fn from(value: &[(Address<NetworkUnchecked>, PendingOutboundValue)]) -> Self {
		Self { inner: Arc::new(RwLock::new(value.iter().cloned().collect())) }
	}
}
