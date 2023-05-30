use cccp_primitives::{
	cli::{Configuration, RelayerConfig, Result as CliResult},
	errors::{INVALID_CONFIG_FILE_PATH, INVALID_CONFIG_FILE_STRUCTURE},
	INVALID_CHAIN_SPECIFICATION,
};

pub fn create_configuration(
	tokio_handle: tokio::runtime::Handle,
	chain: Option<String>,
) -> CliResult<Configuration> {
	let mut config_path = "config.testnet.yaml";
	if let Some(chain) = chain {
		match chain.as_str() {
			"dev" => config_path = "config.testnet.yaml",
			"testnet" => config_path = "config.testnet.yaml",
			"mainnet" => config_path = "config.mainnet.yaml",
			_ => panic!("{}", INVALID_CHAIN_SPECIFICATION),
		}
	}

	let user_config_file =
		std::fs::File::open(format!("configs/{}", config_path)).expect(INVALID_CONFIG_FILE_PATH);
	let user_config: RelayerConfig =
		serde_yaml::from_reader(user_config_file).expect(INVALID_CONFIG_FILE_STRUCTURE);

	Ok(Configuration { relayer_config: user_config, tokio_handle })
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_config_parsing() {
		let user_config_file =
			std::fs::File::open("../../config.yaml").expect(INVALID_CONFIG_FILE_PATH);
		let user_config: RelayerConfig =
			serde_yaml::from_reader(user_config_file).expect(INVALID_CONFIG_FILE_STRUCTURE);

		println!("{:#?}", user_config);
	}
}
