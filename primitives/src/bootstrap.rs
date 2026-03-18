use alloy::{primitives::ChainId, rpc::types::Log};
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
	/// Cached Socket bootstrap logs per chain, populated by SocketQueuePoller and
	/// consumed by SocketRelayHandler to avoid duplicate eth_getLogs calls.
	pub bootstrap_socket_logs: Arc<RwLock<BTreeMap<ChainId, Vec<Log>>>>,
	/// Cached RoundUp bootstrap logs (Bifrost chain only), populated by RoundupEmitter
	/// and consumed by RoundupRelayHandler to avoid a duplicate eth_getLogs call.
	pub bootstrap_roundup_logs: Arc<RwLock<Option<Vec<Log>>>>,
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
		let bootstrap_socket_logs = Arc::new(RwLock::new(BTreeMap::new()));
		let bootstrap_roundup_logs = Arc::new(RwLock::new(None));

		Self {
			roundup_barrier,
			bootstrap_states,
			bootstrap_config: bootstrap_config.clone(),
			bootstrap_socket_logs,
			bootstrap_roundup_logs,
		}
	}
}

impl BootstrapSharedData {
	/// Store fetched bootstrap logs for a chain so subsequent handlers can reuse them.
	pub async fn set_bootstrap_socket_logs(&self, chain_id: ChainId, logs: Vec<Log>) {
		self.bootstrap_socket_logs.write().await.insert(chain_id, logs);
	}

	/// Take the cached bootstrap logs for a chain, removing them from the cache.
	/// Returns `None` if no cache entry exists for the chain.
	pub async fn take_bootstrap_socket_logs(&self, chain_id: ChainId) -> Option<Vec<Log>> {
		self.bootstrap_socket_logs.write().await.remove(&chain_id)
	}

	/// Store the latest RoundUp bootstrap logs (Bifrost chain only).
	/// Called by RoundupEmitter on every get_bootstrap_events() call so the cache
	/// always reflects the most recent fetch when transitioning to Phase2.
	pub async fn set_bootstrap_roundup_logs(&self, logs: Vec<Log>) {
		*self.bootstrap_roundup_logs.write().await = Some(logs);
	}

	/// Take the cached RoundUp bootstrap logs, removing them from the cache.
	/// Returns `None` if RoundupEmitter has not run yet.
	pub async fn take_bootstrap_roundup_logs(&self) -> Option<Vec<Log>> {
		self.bootstrap_roundup_logs.write().await.take()
	}
}
