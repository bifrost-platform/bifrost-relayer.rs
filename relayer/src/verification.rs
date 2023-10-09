use br_primitives::{
	cli::{
		Configuration, DEFAULT_BOOTSTRAP_ROUND_OFFSET, DEFAULT_DUPLICATE_CONFIRM_DELAY_MS,
		DEFAULT_ESCALATE_INTERVAL_SEC, DEFAULT_ESCALATE_PERCENTAGE, DEFAULT_GET_LOGS_BATCH_SIZE,
		MAX_BLOCK_CONFIRMATIONS, MAX_BOOTSTRAP_ROUND_OFFSET, MAX_CALL_INTERVAL_MS,
		MAX_DUPLICATE_CONFIRM_DELAY_MS, MAX_ESCALATE_INTERVAL_SEC, MAX_ESCALATE_PERCENTAGE,
		MIN_GET_LOGS_BATCH_SIZE,
	},
	PARAMETER_OUT_OF_RANGE,
};

/// Verifies whether the certain numeric parameters specified in the configuration YAML file are valid.
/// If any single paramater has been provided, the system will panic on-start.
pub(super) fn assert_configuration_validity(config: &Configuration) {
	let bootstrap_config = &config.relayer_config.bootstrap_config;
	let evm_providers = &config.relayer_config.evm_providers;

	// assert `bootstrap_config`
	if let Some(bootstrap_config) = bootstrap_config {
		if let Some(round_offset) = bootstrap_config.round_offset {
			assert!(
				(0..=MAX_BOOTSTRAP_ROUND_OFFSET).contains(&round_offset),
				"{} [parameter: {}, range: 0…{}, default: {}]",
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
		if let Some(escalate_percentage) = evm_provider.escalate_percentage {
			assert!(
				(f64::from(0)..=MAX_ESCALATE_PERCENTAGE).contains(&escalate_percentage),
				"{} [parameter: {}, range: 0…{}, default: {}]",
				PARAMETER_OUT_OF_RANGE,
				"evm_provider.escalate_percentage",
				MAX_ESCALATE_PERCENTAGE,
				DEFAULT_ESCALATE_PERCENTAGE
			);
		}
		if let Some(escalate_interval) = evm_provider.escalate_interval {
			assert!(
				(0..=MAX_ESCALATE_INTERVAL_SEC).contains(&escalate_interval),
				"{} [parameter: {}, range: 0…{}, default: {}]",
				PARAMETER_OUT_OF_RANGE,
				"evm_provider.escalate_interval",
				MAX_ESCALATE_INTERVAL_SEC,
				DEFAULT_ESCALATE_INTERVAL_SEC
			);
		}
		if let Some(duplicate_confirm_delay) = evm_provider.duplicate_confirm_delay {
			assert!(
				(0..=MAX_DUPLICATE_CONFIRM_DELAY_MS).contains(&duplicate_confirm_delay),
				"{} [parameter: {}, range: 0…{}, default: {}]",
				PARAMETER_OUT_OF_RANGE,
				"evm_provider.duplicate_confirm_delay",
				MAX_DUPLICATE_CONFIRM_DELAY_MS,
				DEFAULT_DUPLICATE_CONFIRM_DELAY_MS
			);
		}
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
