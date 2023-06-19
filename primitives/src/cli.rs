use ethers::types::U64;
use serde::Deserialize;

use crate::eth::ChainID;

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
	pub system: SystemConfig,
	/// BTC config
	pub bitcoin: BitcoinConfig,
	/// EVM configs
	pub evm_providers: Vec<EVMProvider>,
	/// Handler configs
	pub handler_configs: Vec<HandlerConfig>,
	/// Periodic worker configs
	pub periodic_configs: Option<PeriodicWorkerConfig>,
	/// Bootstrapping configs
	pub bootstrap_config: BootstrapConfig,
	/// Sentry config
	pub sentry_config: Option<SentryConfig>,
	/// Prometheus config
	pub prometheus_config: PrometheusConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemConfig {
	/// The identifier of the environment.
	pub id: String,
	/// The private key of the relayer.
	pub private_key: String,
	/// Debug mode enabled if set to `true`.
	pub debug_mode: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BitcoinConfig {
	/// BTC rpc provider url with port.
	pub provider: String,
	/// Username for BTC rpc authentication.
	pub username: String,
	/// Password for BTC rpc authentication.
	pub password: String,
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
	pub block_confirmations: U64,
	/// The flag whether the chain is BIFROST(native) or an external chain.
	pub is_native: Option<bool>,
	/// The flag whether it will handle relay transactions to the current chain.
	pub is_relay_target: bool,
	/// If true, enables Eip1559
	pub eip1559: Option<bool>,
	/// The minimum priority fee required.
	pub min_priority_fee: Option<u64>,
	/// Socket contract address
	pub socket_address: String,
	/// Vault contract address
	pub vault_address: String,
	/// Authority contract address
	pub authority_address: String,
	/// Relayer manager contract address (Only for Bifrost network)
	pub relayer_manager_address: Option<String>,
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
pub struct PriceFeederConfig {
	/// Chain id where oracle contract deployed on
	pub chain_id: ChainID,
	/// Periodic schedule in cron expression.
	pub schedule: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PeriodicWorkerConfig {
	/// Oracle price feeder
	pub oracle_price_feeder: Option<Vec<PriceFeederConfig>>,
	/// Roundup Phase1 feeder
	pub roundup_emitter: String,
	/// Heartbeat sender
	pub heartbeat: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BootstrapConfig {
	/// Bootstrapping flag
	pub is_enabled: bool,
	/// Round for bootstrap
	pub round_offset: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SentryConfig {
	/// The DSN that tells Sentry where to send the events to.
	pub dsn: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrometheusConfig {
	/// Expose a Prometheus exporter endpoint.
	///
	/// Prometheus metric endpoint is enabled by default.
	pub is_enabled: bool,
	/// Expose Prometheus exporter on all interfaces.
	///
	/// Default is local.
	pub is_external: bool,
	/// Prometheus exporter TCP Port.
	pub port: u16,
}
