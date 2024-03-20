use miniscript::bitcoin::address::NetworkUnchecked;
use miniscript::bitcoin::Address;
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct VaultAddressSet {
	inner: Arc<RwLock<BTreeSet<Address<NetworkUnchecked>>>>,
}

impl VaultAddressSet {
	pub fn new() -> Self {
		Self { inner: Default::default() }
	}

	pub async fn insert(&self, value: Address<NetworkUnchecked>) -> bool {
		let mut write_lock = self.inner.write().await;
		write_lock.insert(value)
	}

	pub async fn contains(&self, value: &Address<NetworkUnchecked>) -> bool {
		let read_lock = self.inner.read().await;
		read_lock.contains(value)
	}
}

impl From<BTreeSet<Address<NetworkUnchecked>>> for VaultAddressSet {
	fn from(value: BTreeSet<Address<NetworkUnchecked>>) -> Self {
		Self { inner: Arc::new(RwLock::new(value)) }
	}
}

impl From<&[Address<NetworkUnchecked>]> for VaultAddressSet {
	fn from(value: &[Address<NetworkUnchecked>]) -> Self {
		Self { inner: Arc::new(RwLock::new(value.iter().cloned().collect())) }
	}
}
