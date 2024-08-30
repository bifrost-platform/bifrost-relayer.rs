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
pub use runtime_types::pallet_btc_registration_pool::{
	pallet::pallet::Call::{submit_system_vault_key, submit_vault_key, vault_key_presubmission},
	VaultKeySubmission,
};
pub use runtime_types::pallet_btc_socket_queue::{
	pallet::pallet::Call::submit_unsigned_psbt, RollbackPollMessage, SignedPsbtMessage,
};

pub use bifrost_runtime::btc_registration_pool::calls::types::*;
pub use bifrost_runtime::btc_socket_queue::calls::types::*;
pub use bifrost_runtime::runtime_types::bounded_collections::bounded_vec::BoundedVec;

pub use subxt_signer::eth::{AccountId20, Signature};

impl From<AccountId20> for runtime_types::fp_account::AccountId20 {
	fn from(a: AccountId20) -> Self {
		runtime_types::fp_account::AccountId20(a.0)
	}
}

#[derive(Debug, Clone)]
pub enum CustomConfig {}

impl subxt::Config for CustomConfig {
	type Hash = subxt::utils::H256;
	type AccountId = AccountId20;
	type Address = AccountId20;
	type Signature = Signature;
	type Hasher = subxt::config::substrate::BlakeTwo256;
	type Header =
		subxt::config::substrate::SubstrateHeader<u32, subxt::config::substrate::BlakeTwo256>;
	type ExtrinsicParams = subxt::config::SubstrateExtrinsicParams<Self>;
	type AssetId = u32;
}
