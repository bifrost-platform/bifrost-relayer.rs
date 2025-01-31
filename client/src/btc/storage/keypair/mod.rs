mod kms;
mod password;

pub use kms::KmsKeypairStorage;
pub use password::PasswordKeypairStorage;

use crate::btc::LOG_TARGET;
use aes_gcm::{
	aead::{Aead, KeyInit},
	Aes256Gcm, Key, Nonce,
};
use alloy::primitives::keccak256;
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
use eyre::Result;
use miniscript::bitcoin::{
	psbt::{SigningErrors, SigningKeys},
	Network, Psbt,
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
use tokio::sync::RwLock;

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

#[derive(Clone)]
/// A container for a keystore.
pub struct KeystoreContainer {
	/// The keystore database.
	db: Option<Arc<LocalKeystore>>,
	/// The path to the keystore.
	path: String,
	/// The network of the keystore.
	network: Network,
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

#[async_trait::async_trait]
/// A trait for managing keypairs.
pub trait KeypairStorageT: Send + Sync {
	/// Load the keystore for the given round.
	async fn load(&mut self, round: u32);

	/// Load the keystore for the given round (version 1).
	async fn load_v1(&mut self, round: u32, secret: Option<SecretString>);

	/// Get the database of the keystore.
	async fn db(&self) -> Arc<LocalKeystore>;

	/// Sign the given PSBT.
	async fn sign_psbt(&self, psbt: &mut Psbt) -> bool {
		let before_sign = psbt.clone();
		match self.sign_psbt_inner(psbt).await {
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

	async fn sign_psbt_inner(
		&self,
		psbt: &mut Psbt,
	) -> Result<SigningKeys, (SigningKeys, SigningErrors)>;

	/// Create and store a new keypair.
	async fn create_new_keypair(&self) -> PublicKey;

	/// List public keys.
	async fn keys(&self) -> Vec<Vec<u8>>;

	/// Get the raw keystore value for the given key.
	async fn raw_keystore_value(&self, key: &[u8]) -> Option<String>;

	/// Encrypt the given key using the secret.
	async fn encrypt_key(&self, key: &[u8]) -> Vec<u8>;

	/// Decrypt the given key using the secret.
	async fn decrypt_key(&self, key: &[u8]) -> Vec<u8>;

	/// Insert the given key into the keystore.
	async fn insert_key(&self, key: &[u8], value: &[u8]);

	#[cfg(test)]
	async fn test_get_key(
		&self,
		key_request: KeyRequest,
	) -> Result<Option<PrivateKey>, GetKeyError>;
}

#[derive(Clone)]
/// A wrapper for a keypair manager.
pub struct KeypairStorage(Arc<RwLock<dyn KeypairStorageT>>);

impl KeypairStorage {
	pub fn new(inner: Arc<RwLock<dyn KeypairStorageT>>) -> Self {
		Self(inner)
	}
}

#[async_trait::async_trait]
impl KeypairStorageT for KeypairStorage {
	async fn load(&mut self, round: u32) {
		self.0.write().await.load(round).await;
	}

	async fn load_v1(&mut self, round: u32, secret: Option<SecretString>) {
		self.0.write().await.load_v1(round, secret).await;
	}

	async fn db(&self) -> Arc<LocalKeystore> {
		self.0.read().await.db().await
	}

	async fn sign_psbt_inner(
		&self,
		psbt: &mut Psbt,
	) -> Result<SigningKeys, (SigningKeys, SigningErrors)> {
		self.0.read().await.sign_psbt_inner(psbt).await
	}

	async fn create_new_keypair(&self) -> PublicKey {
		self.0.write().await.create_new_keypair().await
	}

	async fn keys(&self) -> Vec<Vec<u8>> {
		self.0.read().await.keys().await
	}

	async fn raw_keystore_value(&self, key: &[u8]) -> Option<String> {
		self.0.read().await.raw_keystore_value(key).await
	}

	async fn encrypt_key(&self, key: &[u8]) -> Vec<u8> {
		self.0.read().await.encrypt_key(key).await
	}

	async fn decrypt_key(&self, key: &[u8]) -> Vec<u8> {
		self.0.read().await.decrypt_key(key).await
	}

	async fn insert_key(&self, key: &[u8], value: &[u8]) {
		let encrypted_value = self.encrypt_key(value).await;
		self.0.write().await.insert_key(key, &encrypted_value).await;
	}

	#[cfg(test)]
	async fn test_get_key(
		&self,
		key_request: KeyRequest,
	) -> Result<Option<PrivateKey>, GetKeyError> {
		self.0.read().await.test_get_key(key_request).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use miniscript::bitcoin::{psbt::KeyRequest, Network, PublicKey};
	use sc_keystore::Keystore;
	use sp_application_crypto::Pair;
	use sp_core::{crypto::SecretString, testing::ECDSA, ByteArray};
	use std::{str::FromStr as _, sync::Arc};

	#[tokio::test]
	async fn test_encrypt_decrypt() {
		let mut keypair_storage = KeypairStorage::new(PasswordKeypairStorage::new(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		keypair_storage.load(1).await;

		let pair = sp_application_crypto::ecdsa::Pair::generate();
		let public = pair.0.public();
		let private = pair.0.to_raw_vec();

		println!("public -> {:?}", public);
		println!("private -> {:?}", hex::encode(private.clone()));

		let encrypted = keypair_storage.encrypt_key(private.as_slice()).await;
		println!("encrypted -> {:?}", hex::encode(encrypted.clone()));

		let decrypted = keypair_storage.decrypt_key(&encrypted).await;
		println!("decrypted -> {:?}", hex::encode(decrypted));
	}

	#[tokio::test]
	async fn test_decrypt_pw() {
		let mut keypair_storage = KeypairStorage::new(PasswordKeypairStorage::new(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		keypair_storage.load(1).await;

		let keys = keypair_storage.keys().await;
		println!("keys -> {:?}", keys);

		let raw_key = keypair_storage.raw_keystore_value(&keys[0]).await.unwrap();

		println!("public key -> {:?}", hex::encode(keys[0].clone()));

		let decrypted_key =
			keypair_storage.decrypt_key(&hex::decode(raw_key.as_bytes()).unwrap()).await;
		println!("private key -> {:?}", hex::encode(decrypted_key));
	}

	#[tokio::test]
	async fn test_decrypt_kms() {
		let aws_client = Arc::new(aws_sdk_kms::Client::new(
			&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
		));
		let mut keypair_storage = KeypairStorage::new(KmsKeypairStorage::new(
			"../keys".into(),
			Network::Regtest,
			"".into(),
			aws_client,
		));
		keypair_storage.load(1).await;

		let keys = keypair_storage.keys().await;
		println!("keys -> {:?}", keys);

		let raw_key = keypair_storage.raw_keystore_value(&keys[0]).await.unwrap();

		println!("public key -> {:?}", hex::encode(&keys[0]));

		let decrypted_key =
			keypair_storage.decrypt_key(&hex::decode(raw_key.as_bytes()).unwrap()).await;
		println!("private key -> {:?}", hex::encode(decrypted_key));
	}

	#[tokio::test]
	async fn test_create_new_keypair_v1() {
		let mut keypair_storage = KeypairStorage::new(PasswordKeypairStorage::new(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));

		keypair_storage.load_v1(1, Some(SecretString::from_str("test").unwrap())).await;
		let key = keypair_storage.create_new_keypair().await;
		println!("key -> {:?}", key);
	}

	#[tokio::test]
	async fn test_create_new_keypair_pw() {
		let mut keypair_storage = KeypairStorage::new(PasswordKeypairStorage::new(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		keypair_storage.load(1).await;
		keypair_storage.create_new_keypair().await;

		let keys = keypair_storage.keys().await;
		println!("keys -> {:?}", keys);

		let raw_key = keypair_storage.raw_keystore_value(&keys[0]).await.unwrap();
		println!("raw_key -> {:?}", raw_key);

		println!("public key -> {:?}", hex::encode(&keys[0]));

		let decrypted_key =
			keypair_storage.decrypt_key(&hex::decode(raw_key.as_bytes()).unwrap()).await;
		println!("private key -> {:?}", hex::encode(decrypted_key));
	}

	#[tokio::test]
	async fn test_create_new_keypair_kms() {
		let aws_client = Arc::new(aws_sdk_kms::Client::new(
			&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
		));
		let mut keypair_storage = KeypairStorage::new(KmsKeypairStorage::new(
			"../keys".into(),
			Network::Regtest,
			"".into(),
			aws_client,
		));
		keypair_storage.load(1).await;
		keypair_storage.create_new_keypair().await;

		let keys = keypair_storage.keys().await;
		println!("keys -> {:?}", keys);

		let raw_key = keypair_storage.raw_keystore_value(&keys[0]).await.unwrap();
		println!("raw_key -> {:?}", raw_key);

		println!("public key -> {:?}", hex::encode(&keys[0]));

		let decrypted_key =
			keypair_storage.decrypt_key(&hex::decode(raw_key.as_bytes()).unwrap()).await;
		println!("private key -> {:?}", hex::encode(decrypted_key));
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

		let mut keypair_storage = KeypairStorage::new(PasswordKeypairStorage::new(
			"../keys".into(),
			Network::Regtest,
			Some(SecretString::from_str("test").unwrap()),
		));
		keypair_storage.load(1).await;
		for key in keys {
			let pk = PublicKey::from_slice(key.as_slice()).unwrap();
			println!(
				"loaded -> {:?}:{:?}",
				pk,
				keypair_storage.test_get_key(KeyRequest::Pubkey(pk)).await.unwrap()
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
