use ethers::{
	providers::JsonRpcClient,
	types::{Address, H160},
};
use subxt::{blocks::ExtrinsicEvents, OnlineClient};

use crate::btc::storage::keypair::KeypairStorage;
use crate::eth::EthClient;
use bitcoincore_rpc::bitcoin::psbt::Psbt;
use bitcoincore_rpc::bitcoin::secp256k1::Signing;
use ethers::types::Signature as EthSignature;
use std::sync::Arc;

use generic::{
	bifrost_runtime, AccountId20, CustomConfig, EthereumSignature, Public, Signature,
	SignedPsbtMessage, SignedPsbtSubmitted, UnsignedPsbtMessage, UnsignedPsbtSubmitted,
	VaultKeySubmission,
};

pub mod events;
pub mod generic;
pub mod handlers;

#[derive(Clone)]
pub struct BfcClient<T> {
	pub client: OnlineClient<CustomConfig>,
	pub eth_client: Arc<EthClient<T>>,
	keypair_storage: KeypairStorage,
}

impl<T: JsonRpcClient> BfcClient<T> {
	pub fn new(
		client: OnlineClient<CustomConfig>,
		eth_client: Arc<EthClient<T>>,
		keypair_storage: KeypairStorage,
	) -> Result<Self, Box<dyn std::error::Error>> {
		Ok(Self { client, eth_client, keypair_storage })
	}

	pub async fn submit_vault_key(
		&self,
		authority_id: Address, // prime relayer eth address
		who: H160,             // user eth address
	) -> Result<ExtrinsicEvents<CustomConfig>, Box<dyn std::error::Error>> {
		// let authority_id = decode(authority_id).unwrap();
		let pub_key = self.keypair_storage.create_new_keypair().inner.serialize();
		let signature = self
			.convert_ethers_to_ecdsa_signature(self.eth_client.wallet.sign_message(&pub_key))
			.unwrap();

		// `VaultKeySubmission` 구조체 인스턴스 생성
		let vaultkey_submission = VaultKeySubmission {
			authority_id: AccountId20(authority_id.0),
			who: AccountId20(who.0),
			pub_key: Public(pub_key),
		};

		let payload = bifrost_runtime::tx()
			.btc_registration_pool()
			.submit_vault_key(vaultkey_submission, signature);

		// let api: OnlineClient<CustomConfig> = OnlineClient::<CustomConfig>::new().await?;
		let events = self
			.client
			.tx()
			.create_unsigned(&payload)?
			.submit_and_watch()
			.await?
			.wait_for_finalized_success()
			.await?;

		Ok(events)
	}

	pub async fn submit_unsigned_psbt(
		&self,
		authority_id: Address,
		socket_messages: Vec<Vec<u8>>,
		psbt: Psbt,
	) -> Result<ExtrinsicEvents<CustomConfig>, Box<dyn std::error::Error>> {
		let pub_key = self.keypair_storage.create_new_keypair().inner.serialize();
		let signature = self
			.convert_ethers_to_ecdsa_signature(self.eth_client.wallet.sign_message(&pub_key))
			.unwrap();

		let unsigned_msg = UnsignedPsbtMessage {
			authority_id: AccountId20(authority_id.0),
			socket_messages,
			psbt: psbt.serialize(),
		};

		let payload = bifrost_runtime::tx()
			.btc_socket_queue()
			.submit_unsigned_psbt(unsigned_msg, signature);

		let events = self
			.client
			.tx()
			.create_unsigned(&payload)?
			.submit_and_watch()
			.await?
			.wait_for_finalized_success()
			.await?;

		Ok(events)
	}

	pub async fn submit_signed_psbt<C: Signing>(
		&self,
		authority_id: Address,
		unsigned_psbt: Psbt,
	) -> Result<ExtrinsicEvents<CustomConfig>, Box<dyn std::error::Error>> {
		let pub_key = self.keypair_storage.create_new_keypair().inner.serialize();
		let signature = self
			.convert_ethers_to_ecdsa_signature(self.eth_client.wallet.sign_message(&pub_key))
			.unwrap();

		let mut psbt = unsigned_psbt.clone();
		self.keypair_storage.sign_psbt(&mut psbt);

		let signed_msg = SignedPsbtMessage {
			authority_id: AccountId20(authority_id.0),
			unsigned_psbt: unsigned_psbt.serialize(),
			signed_psbt: psbt.serialize(),
		};

		let payload = bifrost_runtime::tx()
			.btc_socket_queue()
			.submit_signed_psbt(signed_msg, signature);

		let events = self
			.client
			.tx()
			.create_unsigned(&payload)?
			.submit_and_watch()
			.await?
			.wait_for_finalized_success()
			.await?;

		Ok(events)
	}

	fn convert_ethers_to_ecdsa_signature(
		&self,
		ethers_signature: EthSignature,
	) -> Result<EthereumSignature, Box<dyn std::error::Error>> {
		// `ethers::types::Signature`에서 `r`과 `s` 값을 추출합니다.

		let sig: String = format!("0x{}", ethers_signature);

		let bytes = sig.as_bytes();

		let mut decode_sig = [0u8; 65];
		decode_sig.copy_from_slice(bytes);

		let signature = EthereumSignature(Signature(decode_sig));
		Ok(signature)
	}
}

#[cfg(all(test, feature = "bfc-client"))]
mod tests {

	use super::*;
	use crate::eth::wallet::WalletManager;
	use br_primitives::{
		cli::{Configuration, EVMProvider, RelayerConfig, Result as CliResult},
		constants::errors::{
			INVALID_CONFIG_FILE_PATH, INVALID_CONFIG_FILE_STRUCTURE, INVALID_PRIVATE_KEY,
			INVALID_PROVIDER_URL,
		},
		eth::{AggregatorContracts, ProtocolContracts, ProviderMetadata},
	};
	use ethers::prelude::{Http, Provider};
	use tokio::time::Duration;

	use bitcoincore_rpc::bitcoin::{
		locktime::absolute, secp256k1::All, transaction, Address, Amount, Network, OutPoint,
		ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
	};

	use std::str::FromStr;

	const TESTNET_CONFIG_FILE_PATH: &str = "configs/config.testnet.yaml";
	// Get this from the output of `bt dumpwallet <file>`.

	const EXTENDED_MASTER_PRIVATE_KEY: &str =
		"701daf3456e8471c4d37cf1752382b5bbfbbd76ea35065e8ec27df6bf4cd926b";

	// Set these with valid data from output of step 5 above. Please note, input utxo must be a p2wpkh.
	const INPUT_UTXO_TXID: &str =
		"295f06639cde6039bf0c3dbf4827f0e3f2b2c2b476408e2f9af731a8d7a9c7fb";
	const INPUT_UTXO_VOUT: u32 = 0;

	const DEFAULT_GET_LOGS_BATCH_SIZE: u64 = 1;

	// Grab an address to receive on: `bt generatenewaddress` (obviously contrived but works as an example).
	const RECEIVE_ADDRESS: &str = "bcrt1qcmnpjjjw78yhyjrxtql6lk7pzpujs3h244p7ae"; // The address to receive the coins we send.

	// These should be correct if the UTXO above should is for 50 BTC.
	const OUTPUT_AMOUNT_BTC: &str = "1 BTC";
	const CHANGE_AMOUNT_BTC: &str = "48.99999 BTC"; // 1000 sat transaction fee.

	async fn default_bfc_client() -> BfcClient<Http> {
		let user_config_file =
			std::fs::File::open(TESTNET_CONFIG_FILE_PATH).expect(INVALID_CONFIG_FILE_PATH);
		let user_config: RelayerConfig =
			serde_yaml::from_reader(user_config_file).expect(INVALID_CONFIG_FILE_STRUCTURE);

		let evm_provider: EVMProvider = user_config.evm_providers.first().unwrap().clone();

		let is_native = evm_provider.is_native.unwrap_or(false);

		let provider = Provider::<Http>::try_from(evm_provider.provider.clone())
			.expect(INVALID_PROVIDER_URL)
			.interval(Duration::from_millis(evm_provider.call_interval));

		let bfc_client = BfcClient::new(
			OnlineClient::<CustomConfig>::new().await.unwrap(),
			Arc::new(EthClient::new(
				WalletManager::from_private_key(EXTENDED_MASTER_PRIVATE_KEY, evm_provider.id)
					.expect(INVALID_PRIVATE_KEY),
				Arc::new(provider.clone()),
				ProviderMetadata::new(
					evm_provider.name.clone(),
					evm_provider.id,
					evm_provider.block_confirmations,
					evm_provider.call_interval,
					evm_provider.get_logs_batch_size.unwrap_or(DEFAULT_GET_LOGS_BATCH_SIZE),
					is_native,
				),
				ProtocolContracts::new(
					Arc::new(provider.clone()),
					evm_provider.socket_address.clone(),
					evm_provider.authority_address.clone(),
					evm_provider.relayer_manager_address.clone(),
				),
				AggregatorContracts::new(
					Arc::new(provider),
					evm_provider.chainlink_usdc_usd_address.clone(),
					evm_provider.chainlink_usdt_usd_address.clone(),
					evm_provider.chainlink_dai_usd_address.clone(),
					evm_provider.chainlink_btc_usd_address.clone(),
					evm_provider.chainlink_wbtc_usd_address.clone(),
				),
				true,
			)),
			KeypairStorage::new(Network::Testnet),
		)
		.unwrap();
		bfc_client
	}

	fn create_psbt() -> Psbt {
		let to_address = Address::from_str(RECEIVE_ADDRESS)
			.unwrap()
			.require_network(Network::Testnet)
			.unwrap();
		let to_amount = Amount::from_str(OUTPUT_AMOUNT_BTC).unwrap();

		let change_amount = Amount::from_str(CHANGE_AMOUNT_BTC).unwrap();

		let tx = Transaction {
			version: transaction::Version::TWO,
			lock_time: absolute::LockTime::ZERO,
			input: vec![TxIn {
				previous_output: OutPoint {
					txid: INPUT_UTXO_TXID.parse().unwrap(),
					vout: INPUT_UTXO_VOUT,
				},
				script_sig: ScriptBuf::new(),
				sequence: Sequence::MAX, // Disable LockTime and RBF.
				witness: Witness::default(),
			}],
			output: vec![TxOut { value: to_amount, script_pubkey: to_address.script_pubkey() }],
		};

		let psbt = Psbt::from_unsigned_tx(tx).unwrap();
		psbt
	}

	#[tokio::test]
	async fn test_submit_vault_key() {
		let test_client = default_bfc_client().await;
		let authority_id = test_client.eth_client.address();
		let who = test_client.eth_client.address();

		test_client.submit_vault_key(authority_id, who).await.unwrap();
	}

	#[tokio::test]
	async fn test_submit_unsigned_psbt() {
		let test_client = default_bfc_client().await;
		let unsigned_psbt = create_psbt();
		let socket_messages = vec![vec![0, 1, 2, 3, 4]];

		test_client
			.submit_unsigned_psbt(test_client.eth_client.address(), socket_messages, unsigned_psbt)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_submit_signed_psbt() {
		let test_client = default_bfc_client().await;
		let unsigned_psbt = create_psbt();

		test_client
			.submit_signed_psbt::<All>(test_client.eth_client.address(), unsigned_psbt)
			.await
			.unwrap();
	}
}
