use crate::Result;
use serde::Deserialize;

pub fn create_configuration(tokio_handle: tokio::runtime::Handle) -> Result<Configuration> {
	let private_config_file =
		std::fs::File::open("config.yaml").expect("Could not open config file.");
	let private_config: RelayerPrivateConfig =
		serde_yaml::from_reader(private_config_file).expect("Config file not valid");

	Ok(Configuration { private_config, tokio_handle })
}

#[derive(Debug)]
pub struct Configuration {
	/// Private things. ex) RPC providers, API keys.
	pub private_config: RelayerPrivateConfig,
	/// Handle to the tokio runtime. Will be used to spawn futures by the task manager.
	pub tokio_handle: tokio::runtime::Handle,
}

#[derive(Debug, Deserialize)]
pub struct RelayerPrivateConfig {
	/// BTC rpc provider url with port.
	pub bitcoin_provider: String,
	/// Username for BTC rpc authentication.
	pub bitcoin_username: String,
	/// Password for BTC rpc authentication.
	pub bitcoin_password: String,

	/// Bifrost rpc provider url.
	pub bfc_provider: String,

	/// Ethereum provider url.
	pub eth_provider: String,

	/// Binance Smart Chain provider url.
	pub bsc_provider: String,

	/// Polygon provider url.
	pub polygon_provider: String,
}
