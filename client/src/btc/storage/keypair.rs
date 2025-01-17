use aws_sdk_kms::{primitives::Blob, Client as KmsClient};
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

/// Load the keystore from the given path and secret.
async fn load(path: String, secret: Option<SecretString>) -> Arc<LocalKeystore> {
	let keystore = LocalKeystore::open(&path, secret).expect(INVALID_KEYSTORE_PASSWORD);
	let keystore = Arc::new(keystore);

	let keys = keystore.keys(ECDSA).expect(INVALID_KEYSTORE_PATH);
	log::info!(
		target: LOG_TARGET,
		"-[{}] 🔐 Keystore loaded (path: {}): {:?} keypairs",
		sub_display_format(SUB_LOG_TARGET),
		&path,
		keys.len()
	);

	keystore
}

/// Sign the given PSBT using the given keystore.
async fn sign_psbt<K: KeypairManager>(keystore: &K, psbt: &mut Psbt) -> bool {
	let before_sign = psbt.clone();
	match psbt.sign(keystore, &Secp256k1::signing_only()) {
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

#[async_trait::async_trait]
/// A trait for managing keypairs.
pub trait KeypairManager: Send + Sync + GetKey {
	/// Load the keystore for the given round.
	async fn load(&mut self, round: u32);

	/// Sign the given PSBT.
	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool;

	/// Create and store a new keypair.
	async fn create_new_keypair(&mut self) -> PublicKey;
}

#[derive(Clone)]
/// A wrapper for a keypair manager.
pub struct KeypairStorage<K: KeypairManager>(pub K);

impl<K: KeypairManager> KeypairStorage<K> {
	pub fn new(inner: K) -> Self {
		Self(inner)
	}
}

#[derive(Clone)]
/// A container for a keystore.
pub struct KeystoreContainer {
	/// The keystore database.
	db: Option<Arc<LocalKeystore>>,
	/// The path to the keystore.
	path: String,
	/// The network of the keystore.
	pub network: Network,
}

impl KeystoreContainer {
	pub fn new(path: String, network: Network) -> Self {
		Self { db: None, path, network }
	}

	pub fn db(&self) -> Arc<LocalKeystore> {
		self.db.clone().expect("Keystore not loaded")
	}
}

#[derive(Clone)]
/// A keystore for password-based keypairs.
pub struct PasswordKeypairStorage {
	/// The keystore container.
	pub inner: KeystoreContainer,
	/// The secret for the keystore.
	pub secret: Option<SecretString>,
}

#[derive(Clone)]
/// A keystore for KMS-based keypairs.
pub struct KmsKeypairStorage {
	/// The keystore container.
	pub inner: KeystoreContainer,
	/// The KMS key ID for encryption/decryption.
	pub key_id: String,
	/// The KMS client.
	pub client: Arc<KmsClient>,
}

#[derive(Clone)]
/// A variant for different types of keystores.
pub enum KeypairStorageKind {
	/// A password-based keystore.
	Password(PasswordKeypairStorage),
	/// A KMS-based keystore.
	Kms(KmsKeypairStorage),
}

impl KeypairStorageKind {
	/// Create a new password-based keystore.
	pub fn new_password(path: String, network: Network, secret: Option<SecretString>) -> Self {
		Self::Password(PasswordKeypairStorage::new(path, network, secret))
	}

	/// Create a new KMS-based keystore.
	pub fn new_kms(path: String, network: Network, key_id: String, client: Arc<KmsClient>) -> Self {
		Self::Kms(KmsKeypairStorage::new(path, network, key_id, client))
	}
}

#[async_trait::async_trait]
impl KeypairManager for KeypairStorageKind {
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

impl PasswordKeypairStorage {
	pub fn new(path: String, network: Network, secret: Option<SecretString>) -> Self {
		Self { inner: KeystoreContainer::new(path, network), secret }
	}
}

impl KmsKeypairStorage {
	pub fn new(path: String, network: Network, key_id: String, client: Arc<KmsClient>) -> Self {
		Self { inner: KeystoreContainer::new(path, network), key_id, client }
	}
}

#[async_trait::async_trait]
impl KeypairManager for PasswordKeypairStorage {
	async fn load(&mut self, round: u32) {
		self.inner.db =
			Some(load(format!("{}/{}", self.inner.path, round), self.secret.clone()).await);
	}

	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		sign_psbt(self, psbt).await
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		let key = self.inner.db().ecdsa_generate_new(ECDSA, None).expect(KEYSTORE_INTERNAL_ERROR);

		if let Err(error) = self.inner.db().key_pair::<AppPair>(
			&AppPublic::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR),
		) {
			panic!("[{}]-[{}] {}: {}", LOG_TARGET, SUB_LOG_TARGET, KEYSTORE_INTERNAL_ERROR, error);
		}
		PublicKey::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR)
	}
}

#[async_trait::async_trait]
impl KeypairManager for KmsKeypairStorage {
	async fn load(&mut self, round: u32) {
		self.inner.db = Some(load(format!("{}/{}", self.inner.path, round), None).await);
	}

	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		sign_psbt(self, psbt).await
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		let pair = sp_application_crypto::ecdsa::Pair::generate();
		let key = pair.0.public();

		let encrypted_key = self
			.client
			.encrypt()
			.key_id(self.key_id.clone())
			.plaintext(Blob::new(pair.0.to_raw_vec().as_slice()))
			.send()
			.await
			.expect("Failed to encrypt");

		self.inner
			.db()
			.insert(
				ECDSA,
				&hex::encode(encrypted_key.ciphertext_blob.unwrap().as_ref()),
				key.as_slice(),
			)
			.expect(KEYSTORE_INTERNAL_ERROR);

		if let Err(error) = self.inner.db().key_pair::<AppPair>(
			&AppPublic::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR),
		) {
			panic!("[{}]-[{}] {}: {}", LOG_TARGET, SUB_LOG_TARGET, KEYSTORE_INTERNAL_ERROR, error);
		}
		PublicKey::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR)
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
				match self.inner.db().key_pair::<AppPair>(
					&AppPublic::from_slice(pk.inner.serialize().as_slice())
						.expect(KEYSTORE_INTERNAL_ERROR),
				) {
					Ok(pair) => {
						if let Some(pair) = pair {
							let mut seed = pair.into_inner().seed();
							let private_key = PrivateKey::from_slice(&seed, self.inner.network)
								.expect(KEYSTORE_INTERNAL_ERROR);
							seed.zeroize();
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

impl GetKey for KmsKeypairStorage {
	type Error = GetKeyError;

	fn get_key<C: Signing>(
		&self,
		key_request: KeyRequest,
		_: &Secp256k1<C>,
	) -> Result<Option<PrivateKey>, Self::Error> {
		match key_request {
			KeyRequest::Pubkey(pk) => {
				match self.inner.db().key_pair::<AppPair>(
					&AppPublic::from_slice(pk.inner.serialize().as_slice())
						.expect(KEYSTORE_INTERNAL_ERROR),
				) {
					Ok(pair) => {
						if let Some(pair) = pair {
							let encrypted_key = hex::decode(pair.into_inner().seed())
								.expect(KEYSTORE_INTERNAL_ERROR);

							let decrypt_result = tokio::runtime::Handle::current()
								.block_on(
									self.client
										.decrypt()
										.key_id(self.key_id.clone())
										.ciphertext_blob(Blob::new(encrypted_key))
										.send(),
								)
								.expect(KEYSTORE_INTERNAL_ERROR);

							let mut decrypted_key = decrypt_result
								.plaintext
								.expect(KEYSTORE_INTERNAL_ERROR)
								.into_inner();

							let private_key =
								PrivateKey::from_slice(&decrypted_key, self.inner.network)
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

#[cfg(test)]
mod tests {
	use super::{KeypairManager, KeypairStorage, KeypairStorageKind};
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
		let keystore =
			sc_keystore::LocalKeystore::open("../keys/1", Some(SecretString::new("test".into())))
				.unwrap();

		let mut keys = vec![];
		for i in 0..3 {
			let key = keystore.ecdsa_generate_new(ECDSA, None).unwrap();
			println!("key{i} -> {:?}", PublicKey::from_slice(key.as_ref()));
			keys.push(key);
		}

		let mut keypair_storage = KeypairStorage::new(KeypairStorageKind::new_password(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		keypair_storage.0.load(1).await;
		for key in keys {
			let pk = PublicKey::from_slice(key.as_slice()).unwrap();
			println!(
				"loaded -> {:?}:{:?}",
				pk,
				keypair_storage
					.0
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
