use ethers::types::U64;
use serde::Deserialize;
use std::{borrow::Cow, fmt::Display};

use crate::eth::ChainID;

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
	/// The identifier of the environment.
	pub id: String,
	/// The private key of the relayer.
	pub private_key: String,
	/// Path of the keystore. (default: `./keys`)
	pub keystore_path: Option<String>,
	/// Password of the keystore. (default: `None`)
	pub keystore_password: Option<String>,
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
	/// The flag whether the chain is Bifrost(native) or an external chain.
	pub is_native: Option<bool>,
	/// The flag whether it will handle relay transactions to the current chain.
	pub is_relay_target: bool,
	/// If true, enables Eip1559. (default: false)
	pub eip1559: Option<bool>,
	/// The minimum value use for gas_price. (default: 0, unit: WEI)
	pub min_gas_price: Option<u64>,
	/// The minimum priority fee required. (default: 0, unit: WEI)
	pub min_priority_fee: Option<u64>,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct BTCProvider {
	/// The bitcoin chain ID used for CCCP.
	pub id: u32,
	/// The Bitcoin provider URL.
	pub provider: String,
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

impl Display for HandlerType {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let str = match *self {
			HandlerType::Socket => "Socket",
			HandlerType::Roundup => "Roundup",
		};
		write!(f, "{}", str)
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
	/// Optional. The round offset used for EVM bootstrap.
	pub round_offset: Option<u32>,
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
pub struct BootstrapConfig {
	/// Bootstrapping flag
	pub is_enabled: bool,
	/// Round for bootstrap
	pub round_offset: Option<u32>,
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
pub struct PriceFeederConfig {
	pub chain_id: u32,
	pub schedule: String,
	pub contract: String,
	pub price_sources: Vec<PriceSource>,
	pub symbols: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PeriodicWorkerConfig {
	/// Oracle price feeder
	pub oracle_price_feeder: Option<Vec<PriceFeederConfig>>,
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
