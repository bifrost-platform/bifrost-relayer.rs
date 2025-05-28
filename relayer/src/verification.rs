use br_primitives::{
	cli::Configuration,
	constants::{
		cli::{
			DEFAULT_BOOTSTRAP_ROUND_OFFSET, DEFAULT_GET_LOGS_BATCH_SIZE, MAX_BLOCK_CONFIRMATIONS,
			MAX_BOOTSTRAP_ROUND_OFFSET, MAX_CALL_INTERVAL_MS, MIN_GET_LOGS_BATCH_SIZE,
		},
		errors::PARAMETER_OUT_OF_RANGE,
	},
};

/// Verifies whether the certain numeric parameters specified in the configuration YAML file are valid.
/// If any single parameter has been provided, the system will panic on-start.
pub(super) fn assert_configuration_validity(config: &Configuration) {
	let bootstrap_config = &config.relayer_config.bootstrap_config;
	let evm_providers = &config.relayer_config.evm_providers;

	// assert `bootstrap_config`
	if let Some(bootstrap_config) = bootstrap_config {
		if let Some(round_offset) = bootstrap_config.round_offset {
			assert!(
				(1..=MAX_BOOTSTRAP_ROUND_OFFSET).contains(&round_offset),
				"{} [parameter: {}, range: 1…{}, default: {}]",
				PARAMETER_OUT_OF_RANGE,
				"bootstrap_config.round_offset",
				MAX_BOOTSTRAP_ROUND_OFFSET,
				DEFAULT_BOOTSTRAP_ROUND_OFFSET
			);
		}
	}

	// assert `evm_providers`
	evm_providers.iter().for_each(|evm_provider| {
		assert!(
			(0..=MAX_CALL_INTERVAL_MS).contains(&evm_provider.call_interval),
			"{} [parameter: {}, range: 0…{}]",
			PARAMETER_OUT_OF_RANGE,
			"evm_provider.call_interval",
			MAX_CALL_INTERVAL_MS
		);
		assert!(
			(0..=MAX_BLOCK_CONFIRMATIONS).contains(&evm_provider.block_confirmations),
			"{} [parameter: {}, range: 0…{}]",
			PARAMETER_OUT_OF_RANGE,
			"evm_provider.block_confirmations",
			MAX_BLOCK_CONFIRMATIONS
		);
		if let Some(get_logs_batch_size) = evm_provider.get_logs_batch_size {
			let max_get_logs_batch_size =
				MAX_CALL_INTERVAL_MS.saturating_div(evm_provider.call_interval);
			assert!(
				(MIN_GET_LOGS_BATCH_SIZE..=max_get_logs_batch_size).contains(&get_logs_batch_size),
				"{} [parameter: {}, range: {}…{}, default: {}]",
				PARAMETER_OUT_OF_RANGE,
				"evm_provider.get_logs_batch_size",
				MIN_GET_LOGS_BATCH_SIZE,
				max_get_logs_batch_size,
				DEFAULT_GET_LOGS_BATCH_SIZE
			);
		}
	});
}
