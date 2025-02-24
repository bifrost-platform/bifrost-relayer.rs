use alloy::primitives::ChainId;
use secrecy::SecretString;
use serde::Deserialize;
use std::borrow::Cow;
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
	pub signer_config: SignerConfig,
	/// Keystore config
	pub keystore_config: Option<KeystoreConfig>,
	/// EVM configs
	pub evm_providers: Vec<EVMProvider>,
	/// BTC configs
	pub btc_provider: BTCProvider,
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
	/// Socket contract address
	pub socket_address: String,
	/// Authority contract address
	pub authority_address: String,
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
	pub password: Option<SecretString>,
	/// AWS KMS key ID. (to use keystore encryption/decryption)
	pub kms_key_id: Option<String>,
}
