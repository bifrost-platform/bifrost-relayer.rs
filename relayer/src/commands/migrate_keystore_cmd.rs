use br_client::btc::storage::keypair::{
	KeypairStorage, KeypairStorageT, KmsKeypairStorage, PasswordKeypairStorage,
};
use br_primitives::{
	cli::Configuration,
	constants::{cli::DEFAULT_KEYSTORE_PATH, errors::INVALID_BITCOIN_NETWORK},
};

use clap::Parser;
use miniscript::bitcoin::Network;
use sc_cli::Error as CliError;
use secrecy::SecretString;
use sp_application_crypto::{
	ecdsa::{AppPair, AppPublic},
	ByteArray,
};
use std::{process::Command, sync::Arc};

use crate::cli::{MAINNET_CONFIG_FILE_PATH, TESTNET_CONFIG_FILE_PATH};

#[derive(Debug, Clone, Parser)]
/// A command to migrate the keystore.
/// This command will migrate the keystore to the new KMS key ID or password.
/// Either `new_kms_key_id` or `new_password` must be provided. (In case both are provided, `new_kms_key_id` will be used.)
/// The specified configuration file will be used to load the keystore. Old credentials must be provided in the configuration file.
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

		let network = Network::from_core_arg(&config.relayer_config.btc_provider.chain)
			.expect(INVALID_BITCOIN_NETWORK);

		let mut password = None;
		let mut keystore_path = DEFAULT_KEYSTORE_PATH.to_string();

		let mut old_keystore = if let Some(keystore_config) = &config.relayer_config.keystore_config
		{
			keystore_path =
				keystore_config.path.clone().unwrap_or(DEFAULT_KEYSTORE_PATH.to_string());

			// 1. Create a new keystore instance with the old credentials.
			if let Some(key_id) = &keystore_config.kms_key_id {
				let aws_client = Arc::new(aws_sdk_kms::Client::new(
					&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
				));
				KeypairStorage::new(KmsKeypairStorage::new(
					keystore_path.clone(),
					network,
					key_id.clone(),
					aws_client,
				))
			} else {
				password = keystore_config.password.clone();
				KeypairStorage::new(PasswordKeypairStorage::new(
					keystore_path.clone(),
					network,
					password.clone(),
				))
			}
		} else {
			KeypairStorage::new(PasswordKeypairStorage::new(
				DEFAULT_KEYSTORE_PATH.to_string(),
				network,
				None,
			))
		};

		// 2. Load the keys from the current (old) keystore.
		if self.version == 1 {
			old_keystore.load_v1(self.round, password.clone()).await;
		} else {
			old_keystore.load(self.round).await;
		}
		let keys = old_keystore.keys().await;

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
			KeypairStorage::new(KmsKeypairStorage::new(
				keystore_path.clone(),
				network,
				key_id.clone(),
				aws_client,
			))
		} else {
			KeypairStorage::new(PasswordKeypairStorage::new(
				keystore_path.clone(),
				network,
				self.new_password.clone(),
			))
		};
		new_keystore.load(self.round).await;

		// 5. Insert the keys to the new keystore.
		for key in keys {
			if self.version == 1 {
				let pair = old_keystore
					.db()
					.await
					.key_pair::<AppPair>(
						&AppPublic::from_slice(&key).expect("Failed to get key pair"),
					)
					.expect("Failed to get key pair")
					.expect("Failed to get key pair");
				new_keystore
					.insert_key(
						&key,
						&new_keystore.encrypt_key(pair.into_inner().seed().as_slice()).await,
					)
					.await;
			} else {
				let value =
					old_keystore.raw_keystore_value(&key).await.expect("Failed to get key pair");
				let decoded_value = hex::decode(value.as_bytes()).expect("Failed to decode");
				let private_key = old_keystore.decrypt_key(&decoded_value).await;
				new_keystore
					.insert_key(&key, &new_keystore.encrypt_key(&private_key).await)
					.await;
			}
		}

		Ok(())
	}
}
