use aes_gcm::{
	aead::{Aead, KeyInit},
	Aes256Gcm, Key, Nonce,
};
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
use pbkdf2::{
	password_hash::{PasswordHasher, SaltString},
	Pbkdf2,
};
use rand::{rngs::OsRng, RngCore};
use sc_keystore::{Keystore, LocalKeystore};
use sp_application_crypto::{
	ecdsa::{AppPair, AppPublic},
	Pair,
};
use sp_core::{
	crypto::{ExposeSecret, SecretString, Zeroize},
	testing::ECDSA,
	ByteArray,
};
use std::sync::Arc;

use crate::btc::LOG_TARGET;

const SUB_LOG_TARGET: &str = "keystore";

/// Load the keystore from the given path.
fn load_keystore(path: String, secret: Option<SecretString>, version: u32) -> Arc<LocalKeystore> {
	let keystore = Arc::new(if version == 1 {
		LocalKeystore::open(&path, secret).expect(INVALID_KEYSTORE_PASSWORD)
	} else {
		LocalKeystore::open(&path, None).expect(INVALID_KEYSTORE_PASSWORD)
	});

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

#[async_trait::async_trait]
/// A trait for managing keypairs.
pub trait KeypairManager: Send + Sync + GetKey {
	/// Load the keystore for the given round.
	fn load(&mut self, round: u32);

	/// Sign the given PSBT.
	fn sign_psbt(&self, psbt: &mut Psbt) -> bool
	where
		Self: Sized,
	{
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

	pub fn load(&mut self, round: u32) {
		self.db = Some(load_keystore(format!("{}/{}", self.path, round), None, 2));
	}

	pub fn load_v1(&mut self, round: u32, secret: Option<SecretString>) {
		self.db = Some(load_keystore(format!("{}/{}", self.path, round), secret, 1));
	}
}

#[derive(Clone)]
/// A keystore for password-based keypairs.
pub struct PasswordKeypairStorage {
	/// The keystore container.
	pub inner: KeystoreContainer,
	/// The secret for encryption/decryption.
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
	fn load(&mut self, round: u32) {
		match self {
			Self::Password(storage) => storage.load(round),
			Self::Kms(storage) => storage.load(round),
		}
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		match self {
			Self::Password(storage) => storage.create_new_keypair().await,
			Self::Kms(storage) => storage.create_new_keypair().await,
		}
	}

	fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		match self {
			Self::Password(storage) => storage.sign_psbt(psbt),
			Self::Kms(storage) => storage.sign_psbt(psbt),
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
	fn load(&mut self, round: u32) {
		self.inner.load(round);
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		let pair = sp_application_crypto::ecdsa::Pair::generate();
		let key = pair.0.public();

		let value = if self.secret.is_some() {
			self.encrypt_key(pair.0.to_raw_vec().as_slice())
		} else {
			pair.0.to_raw_vec()
		};

		self.inner
			.db()
			.insert(ECDSA, &hex::encode(value), key.as_slice())
			.expect(KEYSTORE_INTERNAL_ERROR);

		if let Err(error) = self.inner.db().raw_keystore_value::<AppPair>(
			&AppPublic::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR),
		) {
			panic!("[{}]-[{}] {}: {}", LOG_TARGET, SUB_LOG_TARGET, KEYSTORE_INTERNAL_ERROR, error);
		}
		PublicKey::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR)
	}
}

impl PasswordKeypairStorage {
	/// Encrypt the given key using the secret.
	pub fn encrypt_key(&self, key: &[u8]) -> Vec<u8> {
		if let Some(secret) = &self.secret {
			// Generate a random salt for PBKDF2
			let mut salt = [0u8; 32];
			OsRng.fill_bytes(&mut salt);

			// Generate a random nonce for AES-GCM
			let mut nonce = [0u8; 12];
			OsRng.fill_bytes(&mut nonce);

			let params = pbkdf2::Params {
				rounds: 10_000,
				output_length: 32, // 256 bits
			};
			let password_hash = Pbkdf2
				.hash_password_customized(
					secret.expose_secret().as_bytes(),
					None, // algorithm id
					None, // version
					params,
					&SaltString::encode_b64(&salt).unwrap(),
				)
				.unwrap()
				.hash
				.unwrap();

			// Encrypt the private key
			let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(password_hash.as_bytes()));
			let ciphertext = cipher.encrypt(Nonce::from_slice(&nonce), key.as_ref()).unwrap();

			// Combine salt + nonce + ciphertext for storage
			let mut encrypted_key = Vec::with_capacity(salt.len() + nonce.len() + ciphertext.len());
			encrypted_key.extend_from_slice(&salt);
			encrypted_key.extend_from_slice(&nonce);
			encrypted_key.extend_from_slice(&ciphertext);

			encrypted_key
		} else {
			key.to_vec()
		}
	}

	/// Decrypt the given key using the secret.
	pub fn decrypt_key(&self, key: &[u8]) -> Vec<u8> {
		if let Some(secret) = &self.secret {
			// Split the encrypted data into its components
			let salt = &key[0..32];
			let nonce = &key[32..44];
			let ciphertext = &key[44..];

			let params = pbkdf2::Params { rounds: 10_000, output_length: 32 };

			let password_hash = Pbkdf2
				.hash_password_customized(
					secret.expose_secret().as_bytes(),
					None,
					None,
					params,
					&SaltString::encode_b64(salt).unwrap(),
				)
				.unwrap()
				.hash
				.unwrap();

			// Decrypt the private key
			let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(password_hash.as_bytes()));
			cipher.decrypt(Nonce::from_slice(nonce), ciphertext.as_ref()).unwrap()
		} else {
			key.to_vec()
		}
	}
}

#[async_trait::async_trait]
impl KeypairManager for KmsKeypairStorage {
	fn load(&mut self, round: u32) {
		self.inner.load(round);
	}

	async fn create_new_keypair(&mut self) -> PublicKey {
		let pair = sp_application_crypto::ecdsa::Pair::generate();
		let key = pair.0.public();

		let encrypted_key = self.encrypt_key(pair.0.to_raw_vec().as_slice()).await;

		self.inner
			.db()
			.insert(ECDSA, &hex::encode(encrypted_key), key.as_slice())
			.expect(KEYSTORE_INTERNAL_ERROR);

		if let Err(error) = self.inner.db().raw_keystore_value::<AppPair>(
			&AppPublic::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR),
		) {
			panic!("[{}]-[{}] {}: {}", LOG_TARGET, SUB_LOG_TARGET, KEYSTORE_INTERNAL_ERROR, error);
		}
		PublicKey::from_slice(key.as_slice()).expect(KEYSTORE_INTERNAL_ERROR)
	}
}

impl KmsKeypairStorage {
	/// Encrypt the given key using the KMS client.
	pub async fn encrypt_key(&self, key: &[u8]) -> Vec<u8> {
		let encrypted_key = self
			.client
			.encrypt()
			.key_id(self.key_id.clone())
			.plaintext(Blob::new(key))
			.send()
			.await
			.expect("Failed to encrypt");
		encrypted_key.ciphertext_blob.expect("Failed to encrypt").as_ref().to_vec()
	}

	/// Decrypt the given key using the KMS client.
	pub async fn decrypt_key(&self, key: &[u8]) -> Vec<u8> {
		let decrypt_result = self
			.client
			.decrypt()
			.key_id(self.key_id.clone())
			.ciphertext_blob(Blob::new(key))
			.send()
			.await
			.expect("Failed to decrypt");

		decrypt_result.plaintext.expect("Failed to decrypt").into_inner()
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
				match self.inner.db().raw_keystore_value::<AppPair>(
					&AppPublic::from_slice(pk.inner.serialize().as_slice())
						.expect(KEYSTORE_INTERNAL_ERROR),
				) {
					Ok(value) => {
						if let Some(value) = value {
							let mut seed = if self.secret.is_some() {
								self.decrypt_key(&hex::decode(value.as_bytes()).unwrap())
							} else {
								hex::decode(value.as_bytes()).unwrap()
							};
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
				match self.inner.db().raw_keystore_value::<AppPair>(
					&AppPublic::from_slice(pk.inner.serialize().as_slice())
						.expect(KEYSTORE_INTERNAL_ERROR),
				) {
					Ok(value) => {
						if let Some(value) = value {
							let mut seed = tokio::runtime::Handle::current().block_on(
								self.decrypt_key(&hex::decode(value.as_bytes()).unwrap()),
							);

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

#[cfg(test)]
mod tests {
	use super::{KeypairManager, KeypairStorage, KeypairStorageKind};
	use bitcoincore_rpc::bitcoin::key::Secp256k1;
	use miniscript::bitcoin::{
		psbt::{GetKey, KeyRequest},
		Network, PublicKey,
	};
	use sc_keystore::Keystore;
	use sp_application_crypto::{
		ecdsa::{AppPair, AppPublic},
		Pair,
	};
	use sp_core::{crypto::SecretString, testing::ECDSA, ByteArray};
	use std::str::FromStr as _;

	#[tokio::test]
	async fn test_encrypt_decrypt() {
		let mut keypair_storage = KeypairStorage::new(KeypairStorageKind::new_password(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		keypair_storage.0.load(1);

		let pair = sp_application_crypto::ecdsa::Pair::generate();
		let public = pair.0.public();
		let private = pair.0.to_raw_vec();

		println!("public -> {:?}", public);
		println!("private -> {:?}", hex::encode(private.clone()));

		match keypair_storage.0 {
			KeypairStorageKind::Password(storage) => {
				let encrypted = storage.encrypt_key(private.as_slice());
				println!("encrypted -> {:?}", hex::encode(encrypted.clone()));

				let decrypted = storage.decrypt_key(&encrypted);
				println!("decrypted -> {:?}", hex::encode(decrypted));
			},
			KeypairStorageKind::Kms(_) => {},
		}
	}

	#[tokio::test]
	async fn test_decrypt() {
		let mut keypair_storage = KeypairStorage::new(KeypairStorageKind::new_password(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test2").unwrap()),
		));
		keypair_storage.0.load(1);

		match keypair_storage.0 {
			KeypairStorageKind::Password(storage) => {
				let keys = storage.inner.db().keys(ECDSA).unwrap();
				println!("keys -> {:?}", keys);

				let raw_key = storage
					.inner
					.db()
					.raw_keystore_value::<AppPair>(
						&AppPublic::from_slice(&keys[0].clone()).unwrap(),
					)
					.unwrap()
					.unwrap();

				println!("public key -> {:?}", hex::encode(keys[0].clone()));

				let decrypted_key = storage.decrypt_key(&hex::decode(raw_key.as_bytes()).unwrap());
				println!("private key -> {:?}", hex::encode(decrypted_key));
			},
			KeypairStorageKind::Kms(_) => {},
		}
	}

	#[tokio::test]
	async fn test_create_new_keypair_v1() {
		let keypair_storage = KeypairStorage::new(KeypairStorageKind::new_password(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		match keypair_storage.0 {
			KeypairStorageKind::Password(mut storage) => {
				storage.inner.load_v1(1, Some(SecretString::from_str("test").unwrap()));
				let key = storage.inner.db().ecdsa_generate_new(ECDSA, None).unwrap();
				println!("key -> {:?}", key);
			},
			KeypairStorageKind::Kms(_) => {},
		}
	}

	#[tokio::test]
	async fn test_create_new_keypair() {
		let mut keypair_storage = KeypairStorage::new(KeypairStorageKind::new_password(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		keypair_storage.0.load(1);
		keypair_storage.0.create_new_keypair().await;

		match keypair_storage.0 {
			KeypairStorageKind::Password(storage) => {
				let keys = storage.inner.db().keys(ECDSA).unwrap();
				println!("keys -> {:?}", keys);

				let raw_key = storage
					.inner
					.db()
					.raw_keystore_value::<AppPair>(
						&AppPublic::from_slice(&keys[0].clone()).unwrap(),
					)
					.unwrap()
					.unwrap();
				println!("raw_key -> {:?}", raw_key.as_bytes());

				println!("public key -> {:?}", hex::encode(keys[0].clone()));

				let decrypted_key = storage.decrypt_key(&hex::decode(raw_key.as_bytes()).unwrap());
				println!("private key -> {:?}", hex::encode(decrypted_key));
			},
			KeypairStorageKind::Kms(_) => {},
		}
	}

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
		keypair_storage.0.load(1);
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
