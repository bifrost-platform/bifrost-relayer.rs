use alloy::primitives::ChainId;
use serde::{Deserialize, Deserializer};
use sp_core::crypto::SecretString;
use std::borrow::Cow;

use crate::constants::cli::DEFAULT_SOL_CALL_INTERVAL_MS;

pub type Result<T> = std::result::Result<T, Error>;

/// Error type for the CLI.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum Error {
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error("Invalid input: {0}")]
	Input(String),
}

impl From<&str> for Error {
	fn from(s: &str) -> Error {
		Error::Input(s.to_string())
	}
}

impl From<String> for Error {
	fn from(s: String) -> Error {
		Error::Input(s)
	}
}

#[derive(Debug, Clone)]
pub struct Configuration {
	/// Private things. ex) RPC providers, API keys.
	pub relayer_config: RelayerConfig,
	/// Handle to the tokio runtime. Will be used to spawn futures by the task manager.
	pub tokio_handle: tokio::runtime::Handle,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelayerConfig {
	/// System config
	pub system: Option<SystemConfig>,
	/// Signer config
	pub signer_config: Vec<SignerConfig>,
	/// Keystore config
	pub keystore_config: Option<KeystoreConfig>,
	/// EVM configs
	pub evm_providers: Vec<EVMProvider>,
	/// BTC configs
	pub btc_provider: BTCProvider,
	/// SOL configs
	pub sol_provider: SolProvider,
	/// Handler configs
	pub handler_configs: Vec<HandlerConfig>,
	/// Bootstrapping configs
	pub bootstrap_config: Option<BootstrapConfig>,
	/// Sentry config
	pub sentry_config: Option<SentryConfig>,
	/// Prometheus config
	pub prometheus_config: Option<PrometheusConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemConfig {
	/// Debug mode enabled if set to `true`.
	pub debug_mode: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EVMProvider {
	/// Network name
	pub name: String,
	/// Chain ID
	pub id: ChainId,
	/// Endpoint provider
	pub provider: String,
	/// The time interval(ms) used when to request a new block
	pub call_interval: u64,
	/// The number of confirmations required for a block to be processed.
	pub block_confirmations: u64,
	/// The flag whether the chain is Bifrost(native) or an external chain.
	pub is_native: Option<bool>,
	/// The flag whether it will handle relay transactions to the current chain.
	pub is_relay_target: bool,
	/// If true, enables Eip1559. (default: false)
	pub eip1559: Option<bool>,
	/// The batch size (=block range) used when requesting `eth_getLogs()`. If increased the RPC
	/// request ratio will be reduced, however event processing will be delayed regarded to the
	/// configured batch size. Default size is set to 1, which means it will be requested on every
	/// new block. (default: 1)
	pub get_logs_batch_size: Option<u64>,
	/// Maximum fee (in destination chain's native currency wei) the relayer will pay to execute
	/// a single `Hooks.execute()` call. If the estimated gas cost exceeds this value, the hook
	/// execution is skipped to prevent draining the relayer's native balance.
	/// Only relevant for chains that have `hooks_address` configured.
	pub max_hook_fee: Option<u128>,
	/// Socket contract address
	pub socket_address: String,
	/// Authority contract address
	pub authority_address: String,
	/// Vault contract address
	pub vault_address: String,
	/// Hooks contract address (Only for chains that support hooks)
	pub hooks_address: Option<String>,
	/// Relayer manager contract address (Bifrost only)
	pub relayer_manager_address: Option<String>,
	/// Bitcoin socket contract address (Bifrost only)
	pub bitcoin_socket_address: Option<String>,
	/// Socket Queue contract address (Bifrost only)
	pub socket_queue_address: Option<String>,
	/// Registration Pool contract address (Bifrost only)
	pub registration_pool_address: Option<String>,
	/// Relay Executive contract address (Bifrost only)
	pub relay_executive_address: Option<String>,
	/// Blaze contract address (Bifrost only)
	pub blaze_address: Option<String>,
	/// Chainlink usdc/usd aggregator
	pub chainlink_usdc_usd_address: Option<String>,
	/// Chainlink usdt/usd aggregator
	pub chainlink_usdt_usd_address: Option<String>,
	/// Chainlink dai/usd aggregator
	pub chainlink_dai_usd_address: Option<String>,
	/// Chainlink btc/usd aggregator
	pub chainlink_btc_usd_address: Option<String>,
	/// Chainlink wbtc/usd aggregator
	pub chainlink_wbtc_usd_address: Option<String>,
	/// Chainlink cbbtc/usd aggregator
	pub chainlink_cbbtc_usd_address: Option<String>,
	/// Chainlink jpy/usd aggregator
	pub chainlink_jpy_usd_address: Option<String>,
}

/// Solana cluster configuration. Mirrors `BTCProvider` in spirit but for
/// the `cccp-solana` Anchor program: the relayer polls slots, decodes
/// program logs into `SocketEvent`s, and submits matching Bifrost
/// extrinsics. The `id` field is the same `ChainId` (u64) the rest of the
/// relayer uses to route messages. It is the big-endian numeric form of the
/// 4-byte `ChainIndex` stored in the on-chain `SocketConfig`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolProvider {
	/// CCCP `ChainId` for this cluster — must match the `pallet-cccp-relay-queue`
	/// registration on Bifrost. The logs/metrics name and immutable deployment
	/// identity are derived from this ID.
	pub id: ChainId,
	/// JSON-RPC endpoint URL (`https://api.devnet.solana.com` etc.).
	pub provider: String,
	/// Optional WebSocket endpoint for `slotSubscribe`. Falls back to
	/// polling `getSlot` over the HTTP RPC if absent.
	pub ws_provider: Option<String>,
	/// Slot polling interval in milliseconds.
	#[serde(default = "default_sol_call_interval_ms")]
	pub call_interval: u64,
	/// Whether the relayer should send outbound `poll(...)` IXs to this
	/// cluster. Mirror of `EVMProvider.is_relay_target`. Defaults to false
	/// so an omitted field is watch-only rather than transaction-capable.
	#[serde(default)]
	pub is_relay_target: bool,
	/// Optional `getSignaturesForAddress` page size.
	pub get_signatures_batch_size: Option<u64>,
	/// Path to the local Solana keypair JSON used for fee payment + relayer
	/// signature submissions. Required only when `is_relay_target = true`;
	/// watch-only providers do not load a Solana keypair. The relayer's CCCP
	/// signing key remains secp256k1.
	pub fee_payer_keypair_path: Option<String>,
	/// Base priority fee in micro-lamports per compute unit.
	/// The outbound handler starts at this value and escalates on
	/// confirmation timeouts. Defaults to 1 000.
	pub base_priority_fee: Option<u64>,
	/// Maximum priority fee in micro-lamports per compute unit.
	/// The handler never escalates above this. Defaults to 1 000 000.
	pub max_priority_fee: Option<u64>,
	/// Seconds to wait for a transaction to confirm before
	/// re-sending with a higher priority fee. Defaults to 30.
	pub confirmation_timeout_secs: Option<u64>,
	/// Maximum number of send-with-escalation retries. Defaults to 3.
	pub max_send_retries: Option<u32>,
}

fn default_sol_call_interval_ms() -> u64 {
	DEFAULT_SOL_CALL_INTERVAL_MS
}

#[derive(Debug, Clone, Deserialize)]
pub struct BTCProvider {
	/// The bitcoin chain ID used for CCCP.
	pub id: u64,
	/// The Bitcoin provider URL.
	pub provider: String,
	/// The time interval(ms) used when to request a new block
	pub call_interval: u64,
	/// The number of confirmations required for a block to be processed.
	pub block_confirmations: Option<u64>,
	/// The chain network. (Allowed values: `main`, `test`, `signet`, `regtest`)
	pub chain: String,
	/// Optional. The provider username credential.
	pub username: Option<String>,
	/// Optional. The provider password credential.
	pub password: Option<String>,
	/// Optional. The wallet name.
	pub wallet: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub enum HandlerType {
	/// Socket handler
	Socket,
	/// Roundup handler
	Roundup,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandlerConfig {
	/// Handle type
	pub handler_type: HandlerType,
	/// Watch target list
	pub watch_list: Vec<ChainId>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BootstrapConfig {
	/// Bootstrapping flag
	pub is_enabled: bool,
	/// Optional. The round offset used for EVM bootstrap.
	pub round_offset: Option<u64>,
	/// Optional. The block offset used for Bitcoin bootstrap.
	pub btc_block_offset: Option<u32>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct SentryConfig {
	/// Identifier for Sentry client
	pub environment: Option<Cow<'static, str>>,
	/// Builds a Sentry client.
	pub is_enabled: bool,
	/// The DSN that tells Sentry where to send the events to.
	pub dsn: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrometheusConfig {
	/// Expose a Prometheus exporter endpoint.
	///
	/// Prometheus metric endpoint is disabled by default.
	pub is_enabled: bool,
	/// Expose Prometheus exporter on all interfaces.
	///
	/// Default is local.
	pub is_external: Option<bool>,
	/// Prometheus exporter TCP Port.
	pub port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignerConfig {
	/// AWS KMS key ID. (to use AwsSigner)
	pub kms_key_id: Option<String>,
	/// The private key of the relayer. (to use LocalSigner)
	pub private_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeystoreConfig {
	/// Path of the keystore. (default: `./keys`)
	pub path: Option<String>,
	/// Password of the keystore. (default: `None`)
	#[serde(default, deserialize_with = "deserialize_optional_secret")]
	pub password: Option<SecretString>,
	/// AWS KMS key ID. (to use keystore encryption/decryption)
	pub kms_key_id: Option<String>,
}

/// Deserialize an optional keystore password into a [`SecretString`].
fn deserialize_optional_secret<'de, D>(
	deserializer: D,
) -> std::result::Result<Option<SecretString>, D::Error>
where
	D: Deserializer<'de>,
{
	let raw = Option::<String>::deserialize(deserializer)?;
	Ok(raw.map(SecretString::new))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Deserialize)]
	struct SolProviderOnly {
		sol_provider: SolProvider,
	}

	#[test]
	fn sol_provider_defaults_to_watch_only_and_two_second_polling() {
		let provider: SolProvider = serde_yaml::from_str(
			r#"
id: 1397705728
provider: https://api.devnet.solana.com
"#,
		)
		.unwrap();

		assert!(!provider.is_relay_target);
		assert!(provider.fee_payer_keypair_path.is_none());
		assert_eq!(provider.call_interval, DEFAULT_SOL_CALL_INTERVAL_MS);
	}

	#[test]
	fn sol_provider_config_accepts_one_mapping_not_a_sequence() {
		let config: SolProviderOnly = serde_yaml::from_str(
			r#"
sol_provider:
  id: 1397705728
  provider: https://api.devnet.solana.com
  call_interval: 1000
"#,
		)
		.unwrap();
		assert_eq!(config.sol_provider.id, 1_397_705_728);

		assert!(serde_yaml::from_str::<SolProviderOnly>("{}").is_err());

		let sequence = r#"
sol_provider:
  - id: 1397705728
    provider: https://api.devnet.solana.com
    call_interval: 1000
"#;
		assert!(serde_yaml::from_str::<SolProviderOnly>(sequence).is_err());

		let legacy_sequence = sequence.replace("sol_provider:", "sol_providers:");
		assert!(serde_yaml::from_str::<SolProviderOnly>(&legacy_sequence).is_err());
	}

	#[test]
	fn sol_provider_rejects_removed_static_assets_field() {
		let stale = r#"
id: 1397705728
provider: https://api.devnet.solana.com
assets:
  - index: "0x00"
    mint: So11111111111111111111111111111111111111112
"#;
		let err = serde_yaml::from_str::<SolProvider>(stale).unwrap_err().to_string();
		assert!(err.contains("unknown field `assets`"), "{err}");
	}
}
