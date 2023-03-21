use crate::Result;
use serde::Deserialize;

pub fn create_configuration(tokio_handle: tokio::runtime::Handle) -> Result<Configuration> {
	let private_config_file =
		std::fs::File::open("config.yaml").expect("Could not open config file.");
	let private_config: RelayerPrivateConfig =
		serde_yaml::from_reader(private_config_file).expect("Config file not valid");

	Ok(Configuration { private_config, tokio_handle })
}

#[derive(Debug, Clone)]
pub struct Configuration {
	/// Private things. ex) RPC providers, API keys.
	pub private_config: RelayerPrivateConfig,
	/// Handle to the tokio runtime. Will be used to spawn futures by the task manager.
	pub tokio_handle: tokio::runtime::Handle,
}

impl Configuration {
	pub fn get_evm_config_by_name(&self, name: &str) -> std::result::Result<EVMConfig, String> {
		self.private_config
			.evm_chains
			.iter()
			.find(|evm_config| evm_config.name == name)
			.cloned()
			.ok_or_else(|| format!("EVM config with name {} not found", name))
	}
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelayerPrivateConfig {
	/// BTC config
	pub bitcoin: BitcoinConfig,

	/// EVM configs
	pub evm_chains: Vec<EVMConfig>,
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
pub struct EVMConfig {
	/// Network name
	pub name: String,
	/// Chain ID
	pub id: u32,
	/// Endpoint provider
	pub provider: String,
	/// The time interval used when to request a new block
	pub interval: u64,
	/// The socket contract address
	pub socket: String,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_config_parsing() {
		let private_config_file =
			std::fs::File::open("../../config.yaml").expect("Could not open config file.");
		let private_config: RelayerPrivateConfig =
			serde_yaml::from_reader(private_config_file).expect("Config file not valid");

		println!("{:#?}", private_config);
	}
}
