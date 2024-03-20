use bitcoincore_rpc::bitcoin::key::Secp256k1;
use bitcoincore_rpc::bitcoin::psbt::{GetKey, GetKeyError, KeyRequest};
use bitcoincore_rpc::bitcoin::secp256k1::Signing;
use bitcoincore_rpc::bitcoin::{PrivateKey, PublicKey};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct KeypairStorage {
	inner: Arc<RwLock<BTreeMap<PublicKey, PrivateKey>>>,
}

impl KeypairStorage {
	pub fn new() -> Self {
		Self { inner: Default::default() }
	}

	pub fn insert(&self, key: PublicKey, value: PrivateKey) -> Option<PrivateKey> {
		let mut write_lock = self.inner.blocking_write();
		write_lock.insert(key, value)
	}

	fn get(&self, key: &PublicKey) -> Option<PrivateKey> {
		let read_lock = self.inner.blocking_read();
		read_lock.get(key).cloned()
	}

	async fn sync_db(&self) {
		todo!()
	}
}

impl From<BTreeMap<PublicKey, PrivateKey>> for KeypairStorage {
	fn from(value: BTreeMap<PublicKey, PrivateKey>) -> Self {
		Self { inner: Arc::new(RwLock::new(value)) }
	}
}

impl From<&[(PublicKey, PrivateKey)]> for KeypairStorage {
	fn from(value: &[(PublicKey, PrivateKey)]) -> Self {
		Self { inner: Arc::new(RwLock::new(value.iter().cloned().collect())) }
	}
}

impl GetKey for KeypairStorage {
	type Error = GetKeyError;

	fn get_key<C: Signing>(
		&self,
		key_request: KeyRequest,
		_: &Secp256k1<C>,
	) -> Result<Option<PrivateKey>, Self::Error> {
		match key_request {
			KeyRequest::Pubkey(pk) => Ok(self.get(&pk)),
			_ => Err(GetKeyError::NotSupported),
		}
	}
}
