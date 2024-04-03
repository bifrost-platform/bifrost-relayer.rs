#[subxt::subxt(runtime_metadata_path = "../configs/bifrost_metadata.scale")]
pub mod bifrost_runtime {}

use bifrost_runtime::runtime_types;

pub use runtime_types::bp_multi_sig::Public;
pub use runtime_types::fp_account::{AccountId20, EthereumSignature};
pub use runtime_types::pallet_btc_registration_pool::{
	pallet::pallet::Call::submit_vault_key, VaultKeySubmission,
};
pub use runtime_types::pallet_btc_socket_queue::{
	pallet::pallet::Call::submit_unsigned_psbt, SignedPsbtMessage,
};
pub use runtime_types::sp_core::ecdsa::Signature;

pub use bifrost_runtime::btc_socket_queue::calls::types::SubmitSignedPsbt;

use subxt::config::{Config, DefaultExtrinsicParams, SubstrateConfig};

#[derive(Debug, Clone)]
pub enum CustomConfig {}

impl Config for CustomConfig {
	type Hash = <SubstrateConfig as Config>::Hash;
	type AccountId = <SubstrateConfig as Config>::AccountId;
	type Address = Self::AccountId;
	type Signature = EthereumSignature;
	type Hasher = <SubstrateConfig as Config>::Hasher;
	type Header = <SubstrateConfig as Config>::Header;
	type ExtrinsicParams = DefaultExtrinsicParams<CustomConfig>;
	type AssetId = u32;
}
