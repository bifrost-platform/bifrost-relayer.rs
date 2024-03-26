use ethers::{
	providers::JsonRpcClient,
	types::{Address, H160},
};
use subxt::{blocks::ExtrinsicEvents, OnlineClient};
use tokio::{sync::broadcast::Receiver, time::sleep};

use crate::btc::storage::keypair::KeypairStorage;
use crate::eth::EthClient;
use bitcoincore_rpc::bitcoin::key::Secp256k1;
use bitcoincore_rpc::bitcoin::psbt::Psbt;
use bitcoincore_rpc::bitcoin::secp256k1::Signing;
use bitcoincore_rpc::bitcoin::{PrivateKey, PublicKey};
use bitcoincore_rpc::Error;
use ethers::types::Signature as EthSignature;
use std::collections::BTreeMap;
use std::sync::Arc;

use generic::{
	devnet_runtime, mainnet_runtime, testnet_runtime, AccountId20, CustomConfig, EthereumSignature,
	Public, Signature, SignedPsbtSubmitted, UnsignedPsbtSubmitted, VaultKeySubmission,
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

		let payload = devnet_runtime::tx()
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
		psbt: Psbt,
	) -> Result<ExtrinsicEvents<CustomConfig>, Box<dyn std::error::Error>> {
		let payload =
			devnet_runtime::tx().btc_socket_queue().submit_unsigned_psbt(psbt.serialize());

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
		unsigned_psbt: Psbt,
	) -> Result<ExtrinsicEvents<CustomConfig>, Box<dyn std::error::Error>> {
		let mut psbt = unsigned_psbt.clone();
		self.keypair_storage.sign_psbt(&mut psbt);

		let payload = devnet_runtime::tx()
			.btc_socket_queue()
			.submit_signed_psbt(unsigned_psbt.serialize(), psbt.serialize());

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
