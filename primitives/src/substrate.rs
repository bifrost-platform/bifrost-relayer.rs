#[subxt::subxt(
	runtime_metadata_path = "../configs/bifrost_metadata.scale",
	derive_for_all_types = "Clone",
	derive_for_type(
		path = "bounded_collections::bounded_vec::BoundedVec",
		derive = "Ord, PartialOrd, Eq, PartialEq"
	),
	derive_for_type(path = "bp_btc_relay::MigrationSequence", derive = "Eq, PartialEq")
)]
pub mod bifrost_runtime {}

use bifrost_runtime::runtime_types;

pub use runtime_types::{
	bounded_collections::bounded_vec::BoundedVec,
	bp_btc_relay::{MigrationSequence, Public},
	fp_account::EthereumSignature,
	pallet_blaze::{
		BroadcastSubmission, FeeRateSubmission, OutboundRequestSubmission, UtxoSubmission,
		pallet::pallet::Call::{
			broadcast_poll, submit_fee_rate, submit_outbound_requests, submit_utxos,
		},
	},
	pallet_btc_registration_pool::{
		VaultKeyPreSubmission, VaultKeySubmission,
		pallet::pallet::Call::{
			submit_system_vault_key, submit_vault_key, vault_key_presubmission,
		},
	},
	pallet_btc_socket_queue::{
		RollbackPollMessage, SignedPsbtMessage, pallet::pallet::Call::submit_unsigned_psbt,
	},
};

pub use bifrost_runtime::{
	blaze::calls::types::*, btc_registration_pool::calls::types::*,
	btc_socket_queue::calls::types::*,
};

use super::constants::errors::INVALID_PROVIDER_URL;
use subxt::{
	OnlineClient,
	config::{Config, DefaultExtrinsicParams, SubstrateConfig},
};
use url::Url;

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

pub async fn initialize_sub_client(mut url: Url) -> OnlineClient<CustomConfig> {
	if url.scheme() == "https" {
		url.set_scheme("wss").expect(INVALID_PROVIDER_URL);
	} else {
		url.set_scheme("ws").expect(INVALID_PROVIDER_URL);
	};

	OnlineClient::<CustomConfig>::from_insecure_url(url.as_str())
		.await
		.expect(INVALID_PROVIDER_URL)
}
