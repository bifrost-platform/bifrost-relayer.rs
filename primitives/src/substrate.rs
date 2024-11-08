#[subxt::subxt(
	runtime_metadata_path = "../configs/bifrost_metadata.scale",
	derive_for_all_types = "Clone",
	derive_for_type(
		path = "bounded_collections::bounded_vec::BoundedVec",
		derive = "Ord, PartialOrd, Eq, PartialEq"
	),
	derive_for_type(path = "bp_multi_sig::MigrationSequence", derive = "Eq, PartialEq")
)]
pub mod bifrost_runtime {}

use bifrost_runtime::runtime_types;

pub use runtime_types::bp_multi_sig::{MigrationSequence, Public};
pub use runtime_types::fp_account::{AccountId20, EthereumSignature};
pub use runtime_types::pallet_btc_registration_pool::{
	pallet::pallet::Call::{submit_system_vault_key, submit_vault_key, vault_key_presubmission},
	VaultKeyPreSubmission, VaultKeySubmission,
};
pub use runtime_types::pallet_btc_socket_queue::{
	pallet::pallet::Call::submit_unsigned_psbt, RollbackPollMessage, SignedPsbtMessage,
};
pub use runtime_types::sp_core::ecdsa::Signature;

pub use bifrost_runtime::btc_registration_pool::calls::types::*;
pub use bifrost_runtime::btc_socket_queue::calls::types::*;
pub use bifrost_runtime::runtime_types::bounded_collections::bounded_vec::BoundedVec;

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
