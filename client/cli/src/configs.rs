use br_primitives::{
	cli::{Configuration, RelayerConfig, Result as CliResult},
	errors::{INVALID_CONFIG_FILE_PATH, INVALID_CONFIG_FILE_STRUCTURE},
};

pub fn create_configuration(
	tokio_handle: tokio::runtime::Handle,
	spec: &str,
) -> CliResult<Configuration> {
	let user_config_file = std::fs::File::open(spec).expect(INVALID_CONFIG_FILE_PATH);
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
