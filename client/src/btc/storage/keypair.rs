use aws_sdk_kms::primitives::Blob;
use bitcoincore_rpc::bitcoin::{
	key::Secp256k1,
	psbt::{GetKey, GetKeyError, KeyRequest},
	secp256k1::Signing,
	PrivateKey, PublicKey,
};
use br_primitives::{
	constants::errors::{
		INVALID_KEYSTORE_PASSWORD, INVALID_KEYSTORE_PATH, KEYSTORE_INTERNAL_ERROR,
	},
	utils::sub_display_format,
};
use miniscript::bitcoin::{Network, Psbt};
use sc_keystore::{Keystore, LocalKeystore};
use sp_application_crypto::{
	ecdsa::{AppPair, AppPublic},
	Pair,
};
use sp_core::{
	crypto::{SecretString, Zeroize},
	testing::ECDSA,
	ByteArray,
};
use std::sync::Arc;

use crate::btc::LOG_TARGET;

const SUB_LOG_TARGET: &str = "keystore";

#[async_trait::async_trait]
pub trait KeypairAccessor: Send + Sync + GetKey {
	async fn load(&mut self, round: u32);

	async fn create_new_keypair(&mut self) -> PublicKey;

	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool;
}

#[derive(Clone)]
pub struct KeypairStorage<K: KeypairAccessor> {
	pub inner: K,
}

#[derive(Clone)]
pub struct PasswordKeypairStorage {
	db: Option<Arc<LocalKeystore>>,
	base_path: String,
	network: Network,
	secret: Option<SecretString>,
}

#[derive(Clone)]
pub struct KmsKeypairStorage {
	db: Option<Arc<LocalKeystore>>,
	base_path: String,
	network: Network,
	key_id: String,
	client: Arc<aws_sdk_kms::Client>,
}

#[derive(Clone)]
pub enum KeypairStorageKind {
	Password(PasswordKeypairStorage),
	Kms(KmsKeypairStorage),
}

impl KeypairStorageKind {
	pub fn new_password(base_path: String, network: Network, secret: Option<SecretString>) -> Self {
		Self::Password(PasswordKeypairStorage::new(base_path, network, secret))
	}

	pub fn new_kms(
		base_path: String,
		network: Network,
		key_id: String,
		client: Arc<aws_sdk_kms::Client>,
	) -> Self {
		Self::Kms(KmsKeypairStorage::new(base_path, network, key_id, client))
	}
}

impl<K: KeypairAccessor> KeypairStorage<K> {
	pub fn new(inner: K) -> Self {
		Self { inner }
	}
}

impl PasswordKeypairStorage {
	pub fn new(base_path: String, network: Network, secret: Option<SecretString>) -> Self {
		Self { db: None, base_path, network, secret }
	}

	fn db(&self) -> Arc<LocalKeystore> {
		self.db.clone().expect("Keystore not loaded")
	}
}

impl KmsKeypairStorage {
	pub fn new(
		base_path: String,
		network: Network,
		key_id: String,
		client: Arc<aws_sdk_kms::Client>,
	) -> Self {
		Self { db: None, base_path, network, key_id, client }
	}

	fn db(&self) -> Arc<LocalKeystore> {
		self.db.clone().expect("Keystore not loaded")
	}
}

#[async_trait::async_trait]
impl KeypairAccessor for PasswordKeypairStorage {
	async fn load(&mut self, round: u32) {
		let path = format!("{}/{}", self.base_path, round);

		let keystore =
			LocalKeystore::open(&path, self.secret.clone()).expect(INVALID_KEYSTORE_PASSWORD);
		self.db = Some(Arc::new(keystore));

		let keys = self.db().keys(ECDSA).expect(INVALID_KEYSTORE_PATH);
		log::info!(
			target: LOG_TARGET,
			"-[{}] 🔐 Keystore loaded (path: {}): {:?} keypairs",
			sub_display_format(SUB_LOG_TARGET),
			&path,
			keys.len()
		);
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		let key = self.db().ecdsa_generate_new(ECDSA, None).expect(KEYSTORE_INTERNAL_ERROR);
		let public_key = PublicKey::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR);

		// Ensure the key is stored in the keystore.
		match self.db().key_pair::<AppPair>(
			&AppPublic::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR),
		) {
			Ok(_) => {},
			Err(error) => {
				panic!(
					"[{}]-[{}] {}: {}",
					LOG_TARGET, SUB_LOG_TARGET, KEYSTORE_INTERNAL_ERROR, error
				);
			},
		}

		public_key
	}

	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		let before_sign = psbt.clone();
		match psbt.sign(self, &Secp256k1::signing_only()) {
			Ok(keys) => {
				log::info!(
					target: LOG_TARGET,
					"-[{}] 🔐 Successfully signed psbt. Succeeded({:?})",
					sub_display_format(SUB_LOG_TARGET),
					keys.len()
				);
			},
			Err((keys, errors)) => {
				log::info!(
					target: LOG_TARGET,
					"-[{}] 🔐 Partially signed psbt. Succeeded({:?}) / Failed({:?})",
					sub_display_format(SUB_LOG_TARGET),
					keys.len(),
					errors.len()
				);
			},
		}
		psbt.inputs != before_sign.inputs
	}
}

#[async_trait::async_trait]
impl KeypairAccessor for KmsKeypairStorage {
	async fn load(&mut self, round: u32) {
		let path = format!("{}/{}", self.base_path, round);

		let keystore = LocalKeystore::open(&path, None).expect(INVALID_KEYSTORE_PASSWORD);
		self.db = Some(Arc::new(keystore));

		let keys = self.db().keys(ECDSA).expect(INVALID_KEYSTORE_PATH);
		log::info!(
			target: LOG_TARGET,
			"-[{}] 🔐 Keystore loaded (path: {}): {:?} keypairs",
			sub_display_format(SUB_LOG_TARGET),
			&path,
			keys.len()
		);
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		let pair = sp_application_crypto::ecdsa::Pair::generate();
		let key = pair.0.public();
		let public_key = PublicKey::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR);

		let resp = self
			.client
			.encrypt()
			.key_id(self.key_id.clone())
			.plaintext(Blob::new(pair.0.to_raw_vec().as_slice()))
			.send()
			.await
			.expect("Failed to encrypt");

		let encrypted_private_key = hex::encode(resp.ciphertext_blob.unwrap().as_ref());

		self.db()
			.insert(ECDSA, &encrypted_private_key, key.as_slice())
			.expect(KEYSTORE_INTERNAL_ERROR);

		// Ensure the key is stored in the keystore.
		match self.db().key_pair::<AppPair>(
			&AppPublic::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR),
		) {
			Ok(_) => {},
			Err(error) => {
				panic!(
					"[{}]-[{}] {}: {}",
					LOG_TARGET, SUB_LOG_TARGET, KEYSTORE_INTERNAL_ERROR, error
				);
			},
		}

		public_key
	}

	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		let before_sign = psbt.clone();
		match psbt.sign(self, &Secp256k1::signing_only()) {
			Ok(keys) => {
				log::info!(
					target: LOG_TARGET,
					"-[{}] 🔐 Successfully signed psbt. Succeeded({:?})",
					sub_display_format(SUB_LOG_TARGET),
					keys.len()
				);
			},
			Err((keys, errors)) => {
				log::info!(
					target: LOG_TARGET,
					"-[{}] 🔐 Partially signed psbt. Succeeded({:?}) / Failed({:?})",
					sub_display_format(SUB_LOG_TARGET),
					keys.len(),
					errors.len()
				);
			},
		}
		psbt.inputs != before_sign.inputs
	}
}

impl GetKey for PasswordKeypairStorage {
	type Error = GetKeyError;

	fn get_key<C: Signing>(
		&self,
		key_request: KeyRequest,
		_: &Secp256k1<C>,
	) -> Result<Option<PrivateKey>, Self::Error> {
		match key_request {
			KeyRequest::Pubkey(pk) => {
				match self.db().key_pair::<AppPair>(
					&AppPublic::from_slice(pk.inner.serialize().as_slice())
						.expect(KEYSTORE_INTERNAL_ERROR),
				) {
					Ok(pair) => {
						if let Some(pair) = pair {
							Ok(Some(
								PrivateKey::from_slice(&pair.into_inner().seed(), self.network)
									.expect(KEYSTORE_INTERNAL_ERROR),
							))
						} else {
							Ok(None)
						}
					},
					Err(err) => {
						panic!("{:?}", err)
					},
				}
			},
			_ => Err(GetKeyError::NotSupported),
		}
	}
}

impl GetKey for KmsKeypairStorage {
	type Error = GetKeyError;

	fn get_key<C: Signing>(
		&self,
		key_request: KeyRequest,
		_: &Secp256k1<C>,
	) -> Result<Option<PrivateKey>, Self::Error> {
		match key_request {
			KeyRequest::Pubkey(pk) => {
				match self.db().key_pair::<AppPair>(
					&AppPublic::from_slice(pk.inner.serialize().as_slice())
						.expect(KEYSTORE_INTERNAL_ERROR),
				) {
					Ok(pair) => {
						if let Some(pair) = pair {
							let encrypted_bytes = hex::decode(pair.into_inner().seed())
								.expect(KEYSTORE_INTERNAL_ERROR);

							let decrypt_result = tokio::runtime::Handle::current()
								.block_on(
									self.client
										.decrypt()
										.key_id(self.key_id.clone())
										.ciphertext_blob(Blob::new(encrypted_bytes))
										.send(),
								)
								.expect(KEYSTORE_INTERNAL_ERROR);

							let mut decrypted_key = decrypt_result
								.plaintext
								.expect(KEYSTORE_INTERNAL_ERROR)
								.into_inner();

							let private_key = PrivateKey::from_slice(&decrypted_key, self.network)
								.expect(KEYSTORE_INTERNAL_ERROR);

							decrypted_key.zeroize();

							Ok(Some(private_key))
						} else {
							Ok(None)
						}
					},
					Err(err) => {
						panic!("{:?}", err)
					},
				}
			},
			_ => Err(GetKeyError::NotSupported),
		}
	}
}

#[async_trait::async_trait]
impl KeypairAccessor for KeypairStorageKind {
	async fn load(&mut self, round: u32) {
		match self {
			Self::Password(storage) => storage.load(round).await,
			Self::Kms(storage) => storage.load(round).await,
		}
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		match self {
			Self::Password(storage) => storage.create_new_keypair().await,
			Self::Kms(storage) => storage.create_new_keypair().await,
		}
	}

	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		match self {
			Self::Password(storage) => storage.sign_psbt(psbt).await,
			Self::Kms(storage) => storage.sign_psbt(psbt).await,
		}
	}
}

impl GetKey for KeypairStorageKind {
	type Error = GetKeyError;

	fn get_key<C: Signing>(
		&self,
		key_request: KeyRequest,
		secp: &Secp256k1<C>,
	) -> Result<Option<PrivateKey>, Self::Error> {
		match self {
			Self::Password(storage) => storage.get_key(key_request, secp),
			Self::Kms(storage) => storage.get_key(key_request, secp),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::{KeypairStorage, KeypairStorageKind};
	use bitcoincore_rpc::bitcoin::key::Secp256k1;
	use miniscript::bitcoin::{
		psbt::{GetKey, KeyRequest},
		Network, PublicKey,
	};
	use sc_keystore::Keystore;
	use sp_core::{crypto::SecretString, testing::ECDSA, ByteArray};
	use std::str::FromStr as _;

	#[tokio::test]
	async fn test_load_sc_keystore() {
		let keystore = sc_keystore::LocalKeystore::open(
			"../localkeystore_test",
			Some(SecretString::new("test".into())),
		)
		.unwrap();

		let mut keys = vec![];
		for i in 0..3 {
			let key = keystore.ecdsa_generate_new(ECDSA, None).unwrap();
			println!("key{i} -> {:?}", PublicKey::from_slice(key.as_ref()));
			keys.push(key);
		}

		let keypair_storage = KeypairStorage::new(KeypairStorageKind::new_password(
			"../localkeystore_test".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		for key in keys {
			let pk = PublicKey::from_slice(key.as_slice()).unwrap();
			println!(
				"loaded -> {:?}:{:?}",
				pk,
				keypair_storage
					.inner
					.get_key(KeyRequest::Pubkey(pk), &Secp256k1::signing_only())
					.unwrap()
			);
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
