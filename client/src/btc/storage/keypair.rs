use bitcoincore_rpc::bitcoin::{
	key::Secp256k1,
	psbt::{GetKey, GetKeyError, KeyRequest},
	secp256k1::Signing,
	PrivateKey, PublicKey,
};
use miniscript::bitcoin::{Network, Psbt};
use sc_keystore::LocalKeystore;
use sp_core::crypto::SecretString;
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
		Self {
			inner: Default::default(),
			db: Arc::new(
				LocalKeystore::open(path, Some(SecretString::from_str(secret).unwrap())).unwrap(),
			),
			network,
		}
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
		let ret;
		loop {
			let private_key = PrivateKey::generate(self.network);
			let public_key = private_key.public_key(&Secp256k1::signing_only());
			match self.get(&public_key) {
				Some(_) => continue,
				None => {
					self.insert(public_key.clone(), private_key);
					ret = public_key;
					break;
				},
			}
		}
		ret
	}

	pub fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		let before_sign = psbt.clone();
		psbt.sign(self, &Secp256k1::signing_only()).unwrap();
		psbt.inputs != before_sign.inputs
	}

	async fn sync_db(&self) {
		todo!()
	}
}

impl From<(BTreeMap<PublicKey, PrivateKey>, Arc<LocalKeystore>, Network)> for KeypairStorage {
	fn from(value: (BTreeMap<PublicKey, PrivateKey>, Arc<LocalKeystore>, Network)) -> Self {
		Self { inner: Arc::new(RwLock::new(value.0)), db: value.1, network: value.2 }
	}
}

impl From<(&[(PublicKey, PrivateKey)], Arc<LocalKeystore>, Network)> for KeypairStorage {
	fn from(value: (&[(PublicKey, PrivateKey)], Arc<LocalKeystore>, Network)) -> Self {
		Self {
			inner: Arc::new(RwLock::new(value.0.iter().cloned().collect())),
			db: value.1,
			network: value.2,
		}
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
	use miniscript::bitcoin::{secp256k1::Secp256k1, Network, PrivateKey, PublicKey};
	use sc_keystore::Keystore;
	use sp_application_crypto::ecdsa::{AppPair, AppPublic};
	use sp_core::{crypto::SecretString, testing::ECDSA, ByteArray};
	use std::sync::Arc;

	#[test]
	fn test_load_sc_keystore() {
		let keystore = sc_keystore::LocalKeystore::open(
			"../localkeystore_test",
			Some(SecretString::new("test".to_string())),
		)
		.unwrap();

		for i in 0..3 {
			let key = keystore.ecdsa_generate_new(ECDSA, None).unwrap();
			println!("key{i} -> {:?}", PublicKey::from_slice(key.as_ref()));
		}

		let mut vec = vec![];
		let keys = keystore.keys(ECDSA).unwrap();
		for key in keys.clone() {
			match keystore.key_pair::<AppPair>(&AppPublic::from_slice(&key).unwrap()) {
				Ok(pair) => {
					if let Some(pair) = pair {
						let sk =
							PrivateKey::from_slice(&pair.into_inner().seed(), Network::Regtest)
								.unwrap();
						let pk = PublicKey::from_private_key(&Secp256k1::signing_only(), &sk);
						vec.push((pk, sk));
					} else {
						println!("not found");
					}
				},
				Err(e) => {
					panic!("{e}")
				},
			}
		}

		let keypair_storage =
			KeypairStorage::from((vec.as_slice(), Arc::new(keystore), Network::Regtest));
		for key in keys {
			let pk = PublicKey::from_slice(key.as_slice()).unwrap();
			println!("loaded -> {:?}:{:?}", pk, keypair_storage.get(&pk));
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
