use std::path::Path;
use std::str::FromStr;

use br_primitives::{
	cli::{Configuration, SolProvider},
	constants::{
		cli::{
			DEFAULT_BOOTSTRAP_ROUND_OFFSET, DEFAULT_GET_LOGS_BATCH_SIZE, MAX_BLOCK_CONFIRMATIONS,
			MAX_BOOTSTRAP_ROUND_OFFSET, MAX_CALL_INTERVAL_MS, MIN_GET_LOGS_BATCH_SIZE,
		},
		errors::PARAMETER_OUT_OF_RANGE,
	},
	sol::{SOL_DEV_CHAIN_ID, SOL_MAIN_CHAIN_ID, SOL_TEST_CHAIN_ID, sol_environment},
};

/// Verifies whether the certain numeric parameters specified in the configuration YAML file are valid.
/// If any single parameter has been provided, the system will panic on-start.
pub(super) fn assert_configuration_validity(config: &Configuration) {
	let bootstrap_config = &config.relayer_config.bootstrap_config;
	let evm_providers = &config.relayer_config.evm_providers;
	let sol_provider = &config.relayer_config.sol_provider;

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

	// assert `sol_provider`
	assert_sol_provider_validity(sol_provider);

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

/// Validates one `SolProvider` entry. Panics on misconfiguration so the
/// relayer fails fast at boot — every check below corresponds to a
/// runtime failure mode the operator would otherwise hit hours later.
fn assert_sol_provider_validity(p: &SolProvider) {
	// 1) ChainId selects one immutable Solana environment + deployment.
	let allowed_chain_ids = [SOL_DEV_CHAIN_ID, SOL_TEST_CHAIN_ID, SOL_MAIN_CHAIN_ID];
	let environment = sol_environment(p.id).unwrap_or_else(|| {
		panic!("sol_provider.id must be one of {allowed_chain_ids:?}, got {}", p.id)
	});
	let name = environment.name;
	let deployment = environment.deployment.unwrap_or_else(|| {
		panic!("cccp-solana deployment identity is not configured for ChainId {} ({name})", p.id)
	});

	// 2) JSON-RPC endpoint must parse as a URL
	assert!(
		url::Url::parse(&p.provider).is_ok(),
		"sol_provider.provider for {} is not a valid URL: {}",
		name,
		p.provider
	);
	if let Some(ws) = &p.ws_provider {
		assert!(
			url::Url::parse(ws).is_ok(),
			"sol_provider.ws_provider for {} is not a valid URL: {}",
			name,
			ws
		);
	}

	// 3) call_interval has the same upper bound as the EVM track
	assert!(
		(0..=MAX_CALL_INTERVAL_MS).contains(&p.call_interval),
		"{} [parameter: {}, range: 0…{}]",
		PARAMETER_OUT_OF_RANGE,
		"sol_provider.call_interval",
		MAX_CALL_INTERVAL_MS
	);

	// 4) Compiled deployment identity must remain structurally valid.
	assert!(
		solana_sdk::pubkey::Pubkey::from_str(deployment.program_id).is_ok(),
		"compiled cccp-solana program ID for {} is invalid: {}",
		name,
		deployment.program_id
	);
	assert!(
		solana_sdk::pubkey::Pubkey::from_str(deployment.upgrade_authority).is_ok(),
		"compiled cccp-solana upgrade authority for {} is invalid: {}",
		name,
		deployment.upgrade_authority,
	);
	if p.is_relay_target {
		let fee_payer_path = p.fee_payer_keypair_path.as_deref().unwrap_or_else(|| {
			panic!("sol_provider.fee_payer_keypair_path is required for relay target {name}")
		});
		assert!(
			!fee_payer_path.trim().is_empty(),
			"sol_provider.fee_payer_keypair_path must not be empty for relay target {}",
			name,
		);
		assert!(
			Path::new(fee_payer_path).exists(),
			"sol_provider.fee_payer_keypair_path for {} does not exist: {}",
			name,
			fee_payer_path
		);
	}
	// 5) priority fee sanity: base ≤ max
	if let (Some(base), Some(max)) = (p.base_priority_fee, p.max_priority_fee) {
		assert!(
			base <= max,
			"sol_provider.base_priority_fee ({base}) must not exceed \
			 max_priority_fee ({max}) for {}",
			name
		);
	}
	if let Some(timeout) = p.confirmation_timeout_secs {
		assert!(
			timeout > 0 && timeout <= 300,
			"sol_provider.confirmation_timeout_secs must be 1…300, got {timeout} for {}",
			name
		);
	}
}
