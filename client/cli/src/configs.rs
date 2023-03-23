use cccp_primitives::cli::{Configuration, RelayerConfig, Result as CliResult};

pub fn create_configuration(tokio_handle: tokio::runtime::Handle) -> CliResult<Configuration> {
	let user_config_file = std::fs::File::open("config.yaml").expect("Could not open config file.");
	let user_config: RelayerConfig =
		serde_yaml::from_reader(user_config_file).expect("Config file not valid");

	Ok(Configuration { relayer_config: user_config, tokio_handle })
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
