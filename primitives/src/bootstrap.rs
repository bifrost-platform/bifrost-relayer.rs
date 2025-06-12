use alloy::primitives::ChainId;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{Barrier, RwLock};

use crate::{
	cli::{BootstrapConfig, Configuration},
	eth::BootstrapState,
};

#[derive(Clone)]
pub struct BootstrapSharedData {
	/// The barrier used to lock the system until the roundup bootstrap process is done.
	pub roundup_barrier: Arc<Barrier>,
	/// The current shared state of the bootstrap process.
	pub bootstrap_states: Arc<RwLock<BTreeMap<ChainId, BootstrapState>>>,
	/// The bootstrap configurations.
	pub bootstrap_config: Option<BootstrapConfig>,
}

impl BootstrapSharedData {
	/// Initializes the bootstrap shared data and lock barrier in order to wait until
	/// `Socket` events bootstrap process is completed on each chain.
	pub fn new(config: &Configuration) -> Self {
		let evm_providers = &config.relayer_config.evm_providers;
		let btc_provider = &config.relayer_config.btc_provider;
		let bootstrap_config = &config.relayer_config.bootstrap_config;

		let mut chain_ids = evm_providers.iter().map(|p| p.id).collect::<Vec<_>>();
		chain_ids.push(btc_provider.id);

		fn new_states_with(
			chain_ids: &[ChainId],
			state: BootstrapState,
		) -> BTreeMap<ChainId, BootstrapState> {
			let mut map = BTreeMap::new();
			for chain_id in chain_ids {
				map.insert(*chain_id, state);
			}
			map
		}

		let bootstrap_states = if let Some(bootstrap_config) = bootstrap_config.clone() {
			if bootstrap_config.is_enabled {
				new_states_with(&chain_ids, BootstrapState::NodeSyncing)
			} else {
				new_states_with(&chain_ids, BootstrapState::NormalStart)
			}
		} else {
			new_states_with(&chain_ids, BootstrapState::NormalStart)
		};

		// For roundup barrier, we need to count all external chains
		let roundup_barrier = Arc::new(Barrier::new(
			evm_providers
				.iter()
				.filter(|p| !p.is_native.unwrap_or(false)) // Count all external chains
				.count()
				.saturating_add(1),
		));
		let bootstrap_states = Arc::new(RwLock::new(bootstrap_states));

		Self { roundup_barrier, bootstrap_states, bootstrap_config: bootstrap_config.clone() }
	}
}
