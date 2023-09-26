use serde::Deserialize;
use std::borrow::Cow;

use crate::eth::ChainID;

pub type Result<T> = std::result::Result<T, Error>;

/// The default round offset used on bootstrap. (=3 rounds)
pub const DEFAULT_BOOTSTRAP_ROUND_OFFSET: u32 = 3;

/// The default port used for prometheus.
pub const DEFAULT_PROMETHEUS_PORT: u16 = 8000;

/// The default batch size used for `eth_getLogs()`. (=1 block)
pub const DEFAULT_GET_LOGS_BATCH_SIZE: u64 = 1;

/// The default escalate percentage. (=15%)
pub const DEFAULT_ESCALATE_PERCENTAGE: f64 = 15.0;

/// The default escalate interval in seconds. (=12s)
pub const DEFAULT_ESCALATE_INTERVAL_SEC: u64 = 12;

/// The default duplication confirm delay in milliseconds. (=12s)
pub const DEFAULT_DUPLICATE_CONFIRM_DELAY_MS: u64 = 12_000;

/// The default minimum priority fee in wei. (=0 wei)
pub const DEFAULT_MIN_PRIORITY_FEE: u64 = 0;

/// The default minimum gas price in wei. (=0 wei)
pub const DEFAULT_MIN_GAS_PRICE: u64 = 0;

/// The maximum call interval allowed in milliseconds. (=60s)
pub const MAX_CALL_INTERVAL_MS: u64 = 60_000;

/// The maximum block confirmations allowed. (=100 blocks)
pub const MAX_BLOCK_CONFIRMATIONS: u64 = 100;

/// The maximum escalate percentage allowed. (=100%)
pub const MAX_ESCALATE_PERCENTAGE: f64 = 100.0;

/// The maximum escalate interval allowed in seconds. (=60s)
pub const MAX_ESCALATE_INTERVAL_SEC: u64 = 60;

/// The maximum duplication confirm delay allowed in milliseconds. (=60s)
pub const MAX_DUPLICATE_CONFIRM_DELAY_MS: u64 = 60_000;

/// The minimum batch size allowed for `eth_getLogs()`. (=1 block)
pub const MIN_GET_LOGS_BATCH_SIZE: u64 = 1;

/// The maximum round offset allowed for bootstrap. (=64 rounds)
pub const MAX_BOOTSTRAP_ROUND_OFFSET: u32 = 64;

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
	pub system: SystemConfig,
	/// EVM configs
	pub evm_providers: Vec<EVMProvider>,
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
	/// The private key of the relayer.
	pub private_key: String,
	/// Debug mode enabled if set to `true`.
	pub debug_mode: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EVMProvider {
	/// Network name
	pub name: String,
	/// Chain ID
	pub id: ChainID,
	/// Endpoint provider
	pub provider: String,
	/// The time interval(ms) used when to request a new block
	pub call_interval: u64,
	/// The number of confirmations required for a block to be processed.
	pub block_confirmations: u64,
	/// The flag whether the chain is BIFROST(native) or an external chain.
	pub is_native: Option<bool>,
	/// The flag whether it will handle relay transactions to the current chain.
	pub is_relay_target: bool,
	/// If true, enables Eip1559. (default: false)
	pub eip1559: Option<bool>,
	/// The minimum value use for gas_price. (default: 0, unit: WEI)
	pub min_gas_price: Option<u64>,
	/// The minimum priority fee required. (default: 0, unit: WEI)
	pub min_priority_fee: Option<u64>,
	/// Gas price escalate interval(seconds) when tx stuck in mempool. (default: 12)
	pub escalate_interval: Option<u64>,
	/// Gas price increase percentage on gas price escalation such as when handling tx
	/// replacements. (default: 15.0)
	pub escalate_percentage: Option<f64>,
	/// The flag whether if the gas price will be initially escalated. The `escalate_percentage`
	/// will be used on escalation. This will only have effect on legacy transactions. (default:
	/// false)
	pub is_initially_escalated: Option<bool>,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	pub duplicate_confirm_delay: Option<u64>,
	/// The batch size (=block range) used when requesting `eth_getLogs()`. If increased the RPC
	/// request ratio will be reduced, however event processing will be delayed regarded to the
	/// configured batch size. Default size is set to 1, which means it will be requested on every
	/// new block. (default: 1)
	pub get_logs_batch_size: Option<u64>,
	/// Socket contract address
	pub socket_address: String,
	/// Vault contract address
	pub vault_address: String,
	/// Authority contract address
	pub authority_address: String,
	/// Relayer manager contract address (BIFROST only)
	pub relayer_manager_address: Option<String>,
	/// Chainlink usdc/usd aggregator
	pub chainlink_usdc_usd_address: Option<String>,
	/// Chainlink usdt/usd aggregator
	pub chainlink_usdt_usd_address: Option<String>,
	/// Chainlink dai/usd aggregator
	pub chainlink_dai_usd_address: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub enum HandlerType {
	/// BridgeRelay handler
	BridgeRelay,
	/// Roundup handler
	Roundup,
}

impl ToString for HandlerType {
	fn to_string(&self) -> String {
		match *self {
			HandlerType::BridgeRelay => "BridgeRelay".to_string(),
			HandlerType::Roundup => "Roundup".to_string(),
		}
	}
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandlerConfig {
	/// Handle type
	pub handler_type: HandlerType,
	/// Watch target list
	pub watch_list: Vec<ChainID>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BootstrapConfig {
	/// Bootstrapping flag
	pub is_enabled: bool,
	/// Round for bootstrap
	pub round_offset: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
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
