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
};

/// Verifies whether the certain numeric parameters specified in the configuration YAML file are valid.
/// If any single parameter has been provided, the system will panic on-start.
pub(super) fn assert_configuration_validity(config: &Configuration) {
	let bootstrap_config = &config.relayer_config.bootstrap_config;
	let evm_providers = &config.relayer_config.evm_providers;
	let sol_providers = &config.relayer_config.sol_providers;

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

	// assert `sol_providers`
	for sol_provider in sol_providers {
		assert_sol_provider_validity(sol_provider);
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

/// Validates one `SolProvider` entry. Panics on misconfiguration so the
/// relayer fails fast at boot — every check below corresponds to a
/// runtime failure mode the operator would otherwise hit hours later.
fn assert_sol_provider_validity(p: &SolProvider) {
	// 1) name + chain id must not be empty / zero
	assert!(!p.name.trim().is_empty(), "sol_provider.name must not be empty");
	assert!(p.id != 0, "sol_provider.id must not be zero (cluster {})", p.name);

	// 2) JSON-RPC endpoint must parse as a URL
	assert!(
		url::Url::parse(&p.provider).is_ok(),
		"sol_provider.provider for {} is not a valid URL: {}",
		p.name,
		p.provider
	);
	if let Some(ws) = &p.ws_provider {
		assert!(
			url::Url::parse(ws).is_ok(),
			"sol_provider.ws_provider for {} is not a valid URL: {}",
			p.name,
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

	// 4) commitment, if present, must be one of the canonical levels
	if let Some(c) = &p.commitment {
		let allowed = ["processed", "confirmed", "finalized"];
		assert!(
			allowed.contains(&c.as_str()),
			"sol_provider.commitment for {} must be one of {:?}, got {}",
			p.name,
			allowed,
			c
		);
	}

	// 5) program_id must be a 32-byte base58 pubkey
	assert!(
		solana_sdk::pubkey::Pubkey::from_str(&p.program_id).is_ok(),
		"sol_provider.program_id for {} is not a valid base58 pubkey: {}",
		p.name,
		p.program_id
	);

	// 6) fee_payer keypair file must exist on disk. We do NOT try to
	// load it here because that would surface secret-handling concerns
	// inside the verification path; the actual load happens in
	// `SolOutboundHandler::new` which already returns a structured
	// error. We only check existence so the operator gets a fast,
	// obvious failure for "wrong path" mistakes.
	assert!(
		Path::new(&p.fee_payer_keypair_path).exists(),
		"sol_provider.fee_payer_keypair_path for {} does not exist: {}",
		p.name,
		p.fee_payer_keypair_path
	);

	// 7) priority fee sanity: base ≤ max
	if let (Some(base), Some(max)) = (p.base_priority_fee, p.max_priority_fee) {
		assert!(
			base <= max,
			"sol_provider.base_priority_fee ({base}) must not exceed \
			 max_priority_fee ({max}) for {}",
			p.name
		);
	}
	if let Some(timeout) = p.confirmation_timeout_secs {
		assert!(
			timeout > 0 && timeout <= 300,
			"sol_provider.confirmation_timeout_secs must be 1…300, got {timeout} for {}",
			p.name
		);
	}

	// 8) every asset entry must parse cleanly. We delegate the actual
	// parsing to `AssetRegistry::from_entries` at runtime; here we just
	// check that hex / base58 length is sane so misconfigurations don't
	// silently disable a token.
	for asset in &p.assets {
		let stripped = asset.index.strip_prefix("0x").unwrap_or(&asset.index);
		assert!(
			stripped.len() == 64,
			"sol_provider.assets entry for {} has wrong index length \
			 (expected 64 hex chars, got {}): {}",
			p.name,
			stripped.len(),
			asset.index
		);
		assert!(
			solana_sdk::pubkey::Pubkey::from_str(&asset.mint).is_ok(),
			"sol_provider.assets entry for {} has invalid SPL mint pubkey: {}",
			p.name,
			asset.mint
		);
	}
}
