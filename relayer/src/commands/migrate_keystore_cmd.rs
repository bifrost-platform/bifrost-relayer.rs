use aws_sdk_kms::primitives::Blob;
use br_client::btc::storage::keypair::{KeypairManager, KeypairStorage, KeypairStorageKind};
use br_primitives::{
	cli::Configuration,
	constants::{cli::DEFAULT_KEYSTORE_PATH, errors::INVALID_BITCOIN_NETWORK},
};

use clap::Parser;
use miniscript::bitcoin::Network;
use sc_cli::Error as CliError;
use sc_keystore::Keystore;
use secrecy::SecretString;
use secrecy::Zeroize;
use sp_application_crypto::{
	ecdsa::{AppPair, AppPublic},
	ByteArray,
};
use sp_core::testing::ECDSA;
use std::{process::Command, sync::Arc};

use crate::cli::{MAINNET_CONFIG_FILE_PATH, TESTNET_CONFIG_FILE_PATH};

#[derive(Debug, Clone, Parser)]
/// A command to migrate the keystore.
/// This command will migrate the keystore to the new KMS key ID or password.
/// Either `new_kms_key_id` or `new_password` must be provided. (In case both are provided, `new_kms_key_id` will be used.)
/// The specified configuration file will be used to load the keystore. Old credentials must be provided in the configuration file.
/// # Steps
pub struct MigrateKeystoreCmd {
	#[arg(long)]
	/// The RegistrationPool round to migrate.
	round: u32,

	#[arg(long)]
	/// The new KMS key ID to use for the keystore.
	new_kms_key_id: Option<String>,

	#[arg(long)]
	/// The new password to use for the keystore.
	new_password: Option<SecretString>,

	#[arg(long, value_name = "CHAIN_SPEC")]
	/// The chain specification to use.
	pub chain: String,

	#[arg(long, default_value = "2")]
	/// The keystore version to migrate.
	version: u32,
}

impl MigrateKeystoreCmd {
	/// Chain spec factory
	pub fn load_spec(&self) -> &str {
		match self.chain.as_str() {
			"testnet" => TESTNET_CONFIG_FILE_PATH,
			"mainnet" => MAINNET_CONFIG_FILE_PATH,
			path => path,
		}
	}

	pub async fn run(&self, config: Configuration) -> Result<(), CliError> {
		println!("Migrating keystore for chain: {}", self.chain);

		let keystore_path = config
			.clone()
			.relayer_config
			.keystore_config
			.path
			.unwrap_or(DEFAULT_KEYSTORE_PATH.to_string());
		let network = Network::from_core_arg(&config.relayer_config.btc_provider.chain)
			.expect(INVALID_BITCOIN_NETWORK);

		// 1. Create a new keystore instance with the old credentials.
		let mut old_keystore = if let Some(key_id) = &config.relayer_config.signer_config.kms_key_id
		{
			let aws_client = Arc::new(aws_sdk_kms::Client::new(
				&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
			));
			KeypairStorage::new(KeypairStorageKind::new_kms(
				keystore_path.clone(),
				network,
				key_id.clone(),
				aws_client,
			))
		} else {
			KeypairStorage::new(KeypairStorageKind::new_password(
				keystore_path.clone(),
				network,
				config.relayer_config.keystore_config.password.clone(),
			))
		};

		// 2. Load the keys from the current (old) keystore.
		if self.version == 1 {
			match old_keystore.0 {
				KeypairStorageKind::Password(ref mut storage) => {
					storage.inner.load_v1(
						self.round,
						config.relayer_config.keystore_config.password.clone(),
					);
				},
				KeypairStorageKind::Kms(_) => panic!("KMS keystore is not supported for version 1"),
			}
		} else {
			old_keystore.0.load(self.round);
		}
		let keys = match old_keystore.0.clone() {
			KeypairStorageKind::Password(storage) => {
				storage.inner.db().keys(ECDSA).expect("Failed to load keys")
			},
			KeypairStorageKind::Kms(storage) => {
				storage.inner.db().keys(ECDSA).expect("Failed to load keys")
			},
		};

		// 3. Backup the keystore.
		let backup_path = format!("{}_backup", keystore_path);
		let status = Command::new("cp")
			.args(["-r", &keystore_path, &backup_path])
			.status()
			.expect("Failed to backup the keystore");
		if !status.success() {
			panic!("Failed to backup the keystore");
		}

		// 4. Create a new keystore instance with the new credentials.
		let mut new_keystore = if let Some(key_id) = &self.new_kms_key_id {
			let aws_client = Arc::new(aws_sdk_kms::Client::new(
				&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
			));
			KeypairStorage::new(KeypairStorageKind::new_kms(
				keystore_path.clone(),
				network,
				key_id.clone(),
				aws_client,
			))
		} else {
			KeypairStorage::new(KeypairStorageKind::new_password(
				keystore_path.clone(),
				network,
				self.new_password.clone(),
			))
		};
		new_keystore.0.load(self.round);

		// 5. Insert the keys to the new keystore.
		for key in keys {
			match old_keystore.0 {
				KeypairStorageKind::Password(ref storage) => {
					if self.version == 1 {
						match storage.inner.db().key_pair::<AppPair>(
							&AppPublic::from_slice(&key).expect("Failed to get key pair"),
						) {
							Ok(pair) => {
								if let Some(pair) = pair {
									self.insert_key(
										&new_keystore.0,
										&key,
										pair.into_inner().seed().as_slice(),
									)
									.await;
								} else {
									panic!("Failed to get key pair");
								}
							},
							Err(err) => {
								panic!("Failed to get key pair: {:?}", err)
							},
						}
					} else {
						match storage.inner.db().raw_keystore_value::<AppPair>(
							&AppPublic::from_slice(&key).expect("Failed to get key pair"),
						) {
							Ok(value) => {
								if let Some(value) = value {
									let mut seed = if storage.secret.is_some() {
										storage.decrypt_key(&hex::decode(value.as_bytes()).unwrap())
									} else {
										hex::decode(value.as_bytes()).unwrap()
									};
									self.insert_key(&new_keystore.0, &key, &seed).await;
									seed.zeroize();
								} else {
									panic!("Failed to get key pair");
								}
							},
							Err(err) => {
								panic!("Failed to get key pair: {:?}", err)
							},
						}
					}
				},
				KeypairStorageKind::Kms(ref storage) => {
					match storage.inner.db().raw_keystore_value::<AppPair>(
						&AppPublic::from_slice(&key).expect("Failed to get key pair"),
					) {
						Ok(value) => {
							if let Some(value) = value {
								let mut seed = storage
									.decrypt_key(&hex::decode(value.as_bytes()).unwrap())
									.await;
								self.insert_key(&new_keystore.0, &key, &seed).await;
								seed.zeroize();
							} else {
								panic!("Failed to get key pair");
							}
						},
						Err(err) => {
							panic!("{:?}", err)
						},
					}
				},
			}
		}

		Ok(())
	}

	async fn insert_key(&self, keystore: &KeypairStorageKind, key: &[u8], value: &[u8]) {
		match keystore {
			KeypairStorageKind::Password(ref storage) => {
				let value = if storage.secret.is_some() {
					storage.encrypt_key(value)
				} else {
					value.to_vec()
				};

				storage
					.inner
					.db()
					.insert(ECDSA, &hex::encode(value), &key)
					.expect("Failed to insert key");
			},
			KeypairStorageKind::Kms(ref storage) => {
				let encrypted_key = storage
					.client
					.encrypt()
					.key_id(storage.key_id.clone())
					.plaintext(Blob::new(value))
					.send()
					.await
					.expect("Failed to encrypt");

				storage
					.inner
					.db()
					.insert(
						ECDSA,
						&hex::encode(encrypted_key.ciphertext_blob.unwrap().as_ref()),
						&key,
					)
					.expect("Failed to insert key");
			},
		}
	}
}
