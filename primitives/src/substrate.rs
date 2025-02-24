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

pub use runtime_types::{
	bounded_collections::bounded_vec::BoundedVec,
	bp_multi_sig::{MigrationSequence, Public},
	fp_account::EthereumSignature,
	pallet_btc_registration_pool::{
		VaultKeyPreSubmission, VaultKeySubmission,
		pallet::pallet::Call::{
			submit_system_vault_key, submit_vault_key, vault_key_presubmission,
		},
	},
	pallet_btc_socket_queue::{
		RollbackPollMessage, SignedPsbtMessage, pallet::pallet::Call::submit_unsigned_psbt,
	},
	sp_core::ecdsa::Signature,
};

pub use bifrost_runtime::{
	btc_registration_pool::calls::types::*, btc_socket_queue::calls::types::*,
};

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
