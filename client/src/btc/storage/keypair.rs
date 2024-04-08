use bitcoincore_rpc::bitcoin::{
	key::Secp256k1,
	psbt::{GetKey, GetKeyError, KeyRequest},
	secp256k1::Signing,
	PrivateKey, PublicKey,
};
use miniscript::bitcoin::{Network, Psbt};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct KeypairStorage {
	inner: Arc<RwLock<BTreeMap<PublicKey, PrivateKey>>>,
	network: Network,
}

impl KeypairStorage {
	pub fn new(network: Network) -> Self {
		Self { inner: Default::default(), network }
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

impl From<(BTreeMap<PublicKey, PrivateKey>, Network)> for KeypairStorage {
	fn from(value: (BTreeMap<PublicKey, PrivateKey>, Network)) -> Self {
		Self { inner: Arc::new(RwLock::new(value.0)), network: value.1 }
	}
}

impl From<(&[(PublicKey, PrivateKey)], Network)> for KeypairStorage {
	fn from(value: (&[(PublicKey, PrivateKey)], Network)) -> Self {
		Self { inner: Arc::new(RwLock::new(value.0.iter().cloned().collect())), network: value.1 }
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
	use sc_keystore::Keystore;
	use sp_application_crypto::ecdsa::AppPublic;
	use sp_core::crypto::SecretString;

	#[test]
	fn test_sc_keystore() {
		let keystore = sc_keystore::LocalKeystore::open(
			"../keys",
			Some(SecretString::new("test".to_string())),
		)
		.unwrap();

		let key = keystore.ecdsa_generate_new(sp_core::testing::ECDSA, None).unwrap();
		println!("key -> {:?}", key);

		let keys = keystore.keys(sp_core::testing::ECDSA).unwrap();
		println!("keys -> {:?}", keys);

		let app = AppPublic::from(key);
		println!("app -> {:?}", app);

		match keystore.key_pair::<sp_application_crypto::ecdsa::AppPair>(&AppPublic::from(key)) {
			Ok(pair) => {
				if let Some(pair) = pair {
					println!("seed -> {:?}", pair.clone().into_inner().seed());
					println!(
						"priv.key -> {:?}",
						array_bytes::bytes2hex("0x", pair.into_inner().seed())
					);
				} else {
					println!("not found");
				}
			},
			Err(error) => println!("error -> {:?}", error),
		};
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
