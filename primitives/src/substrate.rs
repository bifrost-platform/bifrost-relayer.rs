#[subxt::subxt(
	runtime_metadata_path = "../configs/bifrost_metadata.scale",
	derive_for_all_types = "Clone",
	derive_for_type(
		path = "bounded_collections::bounded_vec::BoundedVec",
		derive = "Ord, PartialOrd, Eq, PartialEq"
	),
	derive_for_type(path = "bp_multi_sig::MigrationSequence", derive = "Eq, PartialEq"),
	derive_for_type(path = "fp_account::AccountId20", derive = "Copy, Eq, PartialEq")
)]
pub mod bifrost_runtime {}

use bifrost_runtime::runtime_types;

pub use bifrost_runtime::btc_socket_queue::Call as BtcSocketQueueCall;
pub use bifrost_runtime::runtime_types::bifrost_dev_runtime::RuntimeCall as DevRuntimeCall;

pub use runtime_types::bp_multi_sig::{MigrationSequence, Public};
pub use runtime_types::fp_account::{AccountId20, EthereumSignature};
pub use runtime_types::pallet_btc_registration_pool::{
	pallet::pallet::Call::{submit_system_vault_key, submit_vault_key},
	VaultKeySubmission,
};
pub use runtime_types::pallet_btc_socket_queue::{
	pallet::pallet::Call::submit_unsigned_psbt, RollbackPollMessage, RollbackPsbtMessage,
	SignedPsbtMessage,
};
pub use runtime_types::primitive_types::U256 as BifrostU256;
pub use runtime_types::sp_core::ecdsa::Signature;

pub use bifrost_runtime::btc_registration_pool::calls::types::*;
pub use bifrost_runtime::btc_socket_queue::calls::types::*;
pub use bifrost_runtime::runtime_types::bounded_collections::bounded_vec::BoundedVec;

use hex_literal::hex;
use subxt::config::{Config, DefaultExtrinsicParams, SubstrateConfig};
use subxt::tx::Signer;

use libsecp256k1;
use sp_core::ecdsa::Pair as EcdsaPair;
use sp_core::{keccak_256, Pair};
use subxt::utils::{H160, H256};

const ALITH_SEED: &str = "5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133";

#[derive(Debug, Clone)]
pub enum CustomConfig {}

impl Config for CustomConfig {
	type Hash = <SubstrateConfig as Config>::Hash;
	type AccountId = AccountId20;
	type Address = Self::AccountId;
	type Signature = EthereumSignature;
	type Hasher = <SubstrateConfig as Config>::Hasher;
	type Header = <SubstrateConfig as Config>::Header;
	type ExtrinsicParams = DefaultExtrinsicParams<Self>;
	type AssetId = u32;
}

// issue of connect eth signer to customconfig: https://github.com/paritytech/subxt/issues/1180
impl EthPair {
	pub fn alice() -> EthPair {
		let seed_bytes: [u8; 32] = hex::decode(ALITH_SEED)
			.expect("Invalid hex seed")
			.try_into()
			.expect("Invalid length");

		let pair = EcdsaPair::from_seed(&seed_bytes);
		EthPair::from_pair(pair)
	}
}

pub struct EthPair {
	pub account_id: AccountId20,
	pub pair: EcdsaPair,
}

impl EthPair {
	pub fn from_pair(pair: EcdsaPair) -> EthPair {
		let public_key = pair.public();

		let account_id = {
			let decompressed = libsecp256k1::PublicKey::parse_compressed(&public_key.0)
				.expect("Wrong compressed public key provided")
				.serialize();

			let mut m = [0u8; 64];
			m.copy_from_slice(&decompressed[1..65]);
			let account = H160::from(H256::from(sp_core::hashing::keccak_256(&m)));
			let account_id: [u8; 20] = account.into();
			AccountId20(account_id)
		};

		EthPair { account_id, pair }
	}

	pub fn from_string(s: &str) -> EthPair {
		let pair = EcdsaPair::from_string(s, None).expect("valid");
		EthPair::from_pair(pair)
	}
}

impl Signer<CustomConfig> for EthPair {
	fn account_id(&self) -> <CustomConfig as Config>::AccountId {
		self.account_id
	}

	fn address(&self) -> <CustomConfig as Config>::Address {
		self.account_id
	}

	fn sign(&self, signer_payload: &[u8]) -> <CustomConfig as Config>::Signature {
		let message = keccak_256(signer_payload);
		let signature = self.pair.sign_prehashed(&message);

		// Verify the signature
		{
			let m = keccak_256(signer_payload);
			let validity = match sp_io::crypto::secp256k1_ecdsa_recover(signature.as_ref(), &m) {
				Ok(pubkey) => {
					let found_account = AccountId20(H160::from(H256::from(keccak_256(&pubkey))).0);
					found_account == self.account_id
				},
				Err(sp_io::EcdsaVerifyError::BadRS) => false,
				Err(sp_io::EcdsaVerifyError::BadV) => false,
				Err(sp_io::EcdsaVerifyError::BadSignature) => false,
			};
		}

		EthereumSignature(Signature(<[u8; 65]>::from(signature)))
	}
}
