use crate::periodic::PriceSource;
use ethers::types::U64;
use serde::Deserialize;

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

impl Configuration {
	pub fn get_evm_provider_by_name(&self, name: &str) -> std::result::Result<EVMProvider, String> {
		self.relayer_config
			.evm_providers
			.iter()
			.find(|evm_config| evm_config.name == name)
			.cloned()
			.ok_or_else(|| format!("EVM config with name {} not found", name))
	}
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelayerConfig {
	/// BTC config
	pub bitcoin: BitcoinConfig,
	/// EVM configs
	pub evm_providers: Vec<EVMProvider>,
	/// Handler configs
	pub handler_configs: Vec<HandlerConfig>,
	/// The private key of the relayer
	pub private_key: String,
	/// Periodic worker configs
	pub periodic_configs: Option<PeriodicWorkerConfig>,
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
	pub id: u32,
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
}

#[derive(Debug, Clone, Deserialize)]
pub enum HandlerType {
	/// Socket handler
	Socket,
	/// Vault handler
	Vault,
	/// Roundup handler
	Roundup,
}

impl ToString for HandlerType {
	fn to_string(&self) -> String {
		match *self {
			HandlerType::Socket => "Socket".to_string(),
			HandlerType::Vault => "Vault".to_string(),
			HandlerType::Roundup => "Roundup".to_string(),
		}
	}
}

#[derive(Debug, Clone, Deserialize)]
pub struct WatchTarget {
	/// Chain id looking for. Used when mapping EthClient
	pub chain_id: u32,
	/// Contract address looking for.
	pub contract: String,
}

#[derive(Debug, Clone, Deserialize)]
pub enum RoundupHandlerUtilType {
	Socket,
	RelayManager,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoundupHandlerUtilityConfig {
	/// Roundup relay target's util contract type.
	pub contract_type: RoundupHandlerUtilType,
	/// Roundup relay target's util contract address.
	pub contract: String,
	/// Roundup relay target's chain id.
	pub chain_id: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandlerConfig {
	/// Handle type
	pub handler_type: HandlerType,
	/// Watch target list
	pub watch_list: Vec<WatchTarget>,
	/// Roundup relay utils (Only needs for RoundupHandler)
	pub roundup_utils: Option<Vec<RoundupHandlerUtilityConfig>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceFeederConfig {
	/// Chain id where oracle contract deployed on
	pub chain_id: u32,
	/// Periodic schedule in cron expression.
	pub schedule: String,
	/// Oracle contract address
	pub contract: String,
	/// Price source enum. (Coingecko is only available now.)
	pub price_sources: Vec<PriceSource>,
	/// Token/Coin symbols needs to get price
	pub symbols: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoundupEmitterConfig {
	/// Periodic schedule in cron expression.
	pub schedule: String,
	/// Authority contract address (Bifrost network's)
	pub authority_address: String,
	/// Socket contract address (Bifrost network's)
	pub socket_address: String,
	/// RelayerManger contract address (Bifrost network's)
	pub relayer_manager_address: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatSenderConfig {
	/// Periodic schedule in cron expression. (Must be common factor of session duration)
	pub schedule: String,
	/// RelayerManager contract address (Bifrost network's)
	pub relayer_manager_address: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PeriodicWorkerConfig {
	/// Oracle price feeder
	pub oracle_price_feeder: Option<Vec<PriceFeederConfig>>,
	/// Roundup Phase1 feeder
	pub roundup_emitter: RoundupEmitterConfig,
	/// Heartbeat sender
	pub heartbeat: HeartbeatSenderConfig,
}
