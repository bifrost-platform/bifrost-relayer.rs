// #[subxt::subxt(runtime_metadata_insecure_url = "ws://localhost:9944")]
#[subxt::subxt(runtime_metadata_path = "../configs/bifrost_metadata.scale")]
pub mod bifrost_runtime {}

pub use bifrost_runtime::btc_registration_pool::calls::types::SubmitVaultKey;
pub use bifrost_runtime::btc_socket_queue::events::{SignedPsbtSubmitted, UnsignedPsbtSubmitted};
use bifrost_runtime::runtime_types;

pub use runtime_types::pallet_btc_socket_queue::pallet::pallet::Call::submit_unsigned_psbt;

pub use runtime_types::bp_multi_sig::Public;
pub use runtime_types::fp_account::{AccountId20, EthereumSignature};
pub use runtime_types::pallet_btc_registration_pool::VaultKeySubmission;
pub use runtime_types::pallet_btc_socket_queue::{SignedPsbtMessage, UnsignedPsbtMessage};
pub use runtime_types::sp_core::ecdsa::Signature;

use subxt::config::{Config, DefaultExtrinsicParams, PolkadotConfig, SubstrateConfig};

use std::{
	fs,
	path::Path,
	process::{Child, Command},
};

#[derive(Debug, Clone)]
pub enum CustomConfig {}

impl Config for CustomConfig {
	type Hash = <SubstrateConfig as Config>::Hash;
	type AccountId = <SubstrateConfig as Config>::AccountId;
	type Address = <PolkadotConfig as Config>::Address;
	type Signature = <SubstrateConfig as Config>::Signature;
	type Hasher = <SubstrateConfig as Config>::Hasher;
	type Header = <SubstrateConfig as Config>::Header;
	type ExtrinsicParams = DefaultExtrinsicParams<CustomConfig>;
	type AssetId = u32;
}

pub async fn generate_metadata() -> anyhow::Result<()> {
	let output = Command::new("subxt")
		.arg("metadata")
		.arg("-f")
		.arg("bytes")
		.arg(">")
		.output()
		.expect("failed to execute process");

	let metadata_path = Path::new("./configs/bifrost_metadata.scale");
	fs::write(metadata_path, &output.stdout).unwrap();
	Ok(())
}
