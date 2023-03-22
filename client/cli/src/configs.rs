use crate::Result;
use serde::Deserialize;

pub fn create_configuration(tokio_handle: tokio::runtime::Handle) -> Result<Configuration> {
	let user_config_file = std::fs::File::open("config.yaml").expect("Could not open config file.");
	let user_config: RelayerConfig =
		serde_yaml::from_reader(user_config_file).expect("Config file not valid");

	Ok(Configuration { relayer_config: user_config, tokio_handle })
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
	/// Targets
	pub watch_targets: Vec<WatchTarget>,
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
}

#[derive(Debug, Clone, Deserialize)]
pub enum HandlerType {
	/// Socket handler
	Socket,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TargetInfo {
	/// Chain id looking for. Used when mapping EthClient
	pub chain_id: u32,
	/// Contract address looking for.
	pub contract: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WatchTarget {
	/// Handle type
	pub handler_type: HandlerType,
	/// Target list
	pub target: Vec<TargetInfo>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_config_parsing() {
		let user_config_file =
			std::fs::File::open("../../config.yaml").expect("Could not open config file.");
		let user_config: RelayerConfig =
			serde_yaml::from_reader(user_config_file).expect("Config file not valid");

		println!("{:#?}", user_config);
	}
}
