use bitcoincore_rpc::bitcoin::{
	key::Secp256k1,
	psbt::{GetKey, GetKeyError, KeyRequest},
	secp256k1::Signing,
	PrivateKey, PublicKey,
};
use miniscript::bitcoin::{Network, Psbt};
use sc_keystore::{Keystore, LocalKeystore};
use sp_application_crypto::ecdsa::{AppPair, AppPublic};
use sp_core::{crypto::SecretString, testing::ECDSA, ByteArray};
use std::{collections::BTreeMap, str::FromStr, sync::Arc};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct KeypairStorage {
	inner: Arc<RwLock<BTreeMap<PublicKey, PrivateKey>>>,
	db: Arc<LocalKeystore>,
	network: Network,
}

impl KeypairStorage {
	pub fn new(path: &str, secret: &str, network: Network) -> Self {
		let keystore =
			LocalKeystore::open(path, Some(SecretString::from_str(secret).unwrap())).unwrap();
		let mut inner = BTreeMap::new();

		let keys = keystore.keys(ECDSA).unwrap();
		for key in keys.clone() {
			match keystore.key_pair::<AppPair>(&AppPublic::from_slice(&key).unwrap()) {
				Ok(pair) => {
					if let Some(pair) = pair {
						let sk =
							PrivateKey::from_slice(&pair.into_inner().seed(), network).unwrap();
						let pk = PublicKey::from_private_key(&Secp256k1::signing_only(), &sk);
						inner.insert(pk, sk);
					}
				},
				Err(e) => {
					panic!("{e}")
				},
			}
		}

		Self { inner: Arc::new(RwLock::new(inner)), db: Arc::new(keystore), network }
	}

	fn insert(&self, key: PublicKey, value: PrivateKey) -> Option<PrivateKey> {
		let mut write_lock = self.inner.blocking_write();
		write_lock.insert(key, value)
	}

	fn get(&self, key: &PublicKey) -> Option<PrivateKey> {
		let read_lock = self.inner.blocking_read();
		read_lock.get(key).cloned()
	}

	pub fn create_new_keypair(&self) -> PublicKey {
		let key = self.db.ecdsa_generate_new(ECDSA, None).unwrap();
		let public_key = PublicKey::from_slice(key.as_slice()).unwrap();

		match self.db.key_pair::<AppPair>(&AppPublic::from_slice(&key.as_slice()).unwrap()) {
			Ok(pair) => {
				if let Some(pair) = pair {
					self.insert(
						public_key,
						PrivateKey::from_slice(&pair.into_inner().seed(), self.network).unwrap(),
					);
				}
			},
			Err(e) => {
				panic!("{e}")
			},
		}

		public_key
	}

	pub fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		let before_sign = psbt.clone();
		psbt.sign(self, &Secp256k1::signing_only()).unwrap();
		psbt.inputs != before_sign.inputs
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

#[cfg(test)]
mod tests {
	use super::KeypairStorage;
	use miniscript::bitcoin::{Network, PublicKey};
	use sc_keystore::Keystore;
	use sp_core::{crypto::SecretString, testing::ECDSA, ByteArray};

	#[test]
	fn test_load_sc_keystore() {
		let keystore = sc_keystore::LocalKeystore::open(
			"../localkeystore_test",
			Some(SecretString::new("test".to_string())),
		)
		.unwrap();

		let mut keys = vec![];
		for i in 0..3 {
			let key = keystore.ecdsa_generate_new(ECDSA, None).unwrap();
			println!("key{i} -> {:?}", PublicKey::from_slice(key.as_ref()));
			keys.push(key);
		}

		let keypair_storage =
			KeypairStorage::new("../localkeystore_test", "test", Network::Regtest);
		for key in keys {
			let pk = PublicKey::from_slice(key.as_slice()).unwrap();
			println!("loaded -> {:?}:{:?}", pk, keypair_storage.get(&pk).unwrap());
		}
	}

	#[test]
	fn test_array_bytes_to_hex() {
		let a = [
			145, 25, 69, 113, 185, 112, 97, 20, 82, 14, 62, 68, 237, 185, 221, 252, 184, 230, 7,
			128, 53, 132, 90, 165, 110, 159, 153, 213, 245, 90, 181, 155,
		];
		println!("a.len -> {:?}", a.len());
		let b = array_bytes::bytes2hex("0x", a);
		println!("b -> {:?}", b);
	}
}
