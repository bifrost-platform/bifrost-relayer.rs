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
	pub mnemonic: String,
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
	pub interval: u64,
	/// True if bifrost network. False for else networks.
	pub is_native: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub enum HandlerType {
	/// Socket handler
	Socket,
	/// Vault handler
	Vault,
}

impl ToString for HandlerType {
	fn to_string(&self) -> String {
		match *self {
			HandlerType::Socket => "Socket".to_string(),
			HandlerType::Vault => "Vault".to_string(),
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
pub struct HandlerConfig {
	/// Handle type
	pub handler_type: HandlerType,
	/// Target list
	pub watch_list: Vec<WatchTarget>,
}
