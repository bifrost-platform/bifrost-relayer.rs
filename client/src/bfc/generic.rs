#[subxt::subxt(runtime_metadata_insecure_url = "ws://localhost:9944")]
pub mod devnet_runtime {}

#[subxt::subxt(runtime_metadata_insecure_url = "wss://public-01.testnet.bifrostnetwork.com/wss")]
pub mod testnet_runtime {}

#[subxt::subxt(runtime_metadata_insecure_url = "wss://public-01.mainnet.bifrostnetwork.com/wss")]
pub mod mainnet_runtime {}

pub use devnet_runtime::btc_socket_queue::events::{SignedPsbtSubmitted, UnsignedPsbtSubmitted};
use devnet_runtime::runtime_types;

pub use runtime_types::fp_account::{AccountId20, EthereumSignature};
pub use runtime_types::pallet_btc_registration_pool::{Public, VaultKeySubmission};
pub use runtime_types::sp_core::ecdsa::Signature;

use subxt::config::{Config, DefaultExtrinsicParams, PolkadotConfig, SubstrateConfig};

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
