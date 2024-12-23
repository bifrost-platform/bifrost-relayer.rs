use std::sync::Arc;
use tokio::sync::{Barrier, Mutex, RwLock};

use crate::{
	cli::{BootstrapConfig, Configuration},
	eth::BootstrapState,
};

#[derive(Clone)]
pub struct BootstrapSharedData {
	/// The evm + non-evm providers length.
	pub system_providers_len: usize,
	/// The barrier used to lock the system until the roundup bootstrap process is done.
	pub roundup_barrier: Arc<Barrier>,
	/// The current number of finished socket bootstrap processes.
	pub socket_bootstrap_count: Arc<Mutex<u8>>,
	/// The current shared state of the bootstrap process.
	pub bootstrap_state: Arc<RwLock<BootstrapState>>,
	/// The bootstrap configurations.
	pub bootstrap_config: Option<BootstrapConfig>,
}

impl BootstrapSharedData {
	/// Initializes the bootstrap shared data and lock barrier in order to wait until
	/// `Socket` events bootstrap process is completed on each chain.
	pub fn new(config: &Configuration) -> Self {
		let evm_providers = &config.relayer_config.evm_providers;
		let bootstrap_config = &config.relayer_config.bootstrap_config;

		let system_providers_len = evm_providers.len().saturating_add(1); // add 1 for Bitcoin

		let bootstrap_state = if let Some(bootstrap_config) = bootstrap_config.clone() {
			if bootstrap_config.is_enabled {
				BootstrapState::NodeSyncing
			} else {
				BootstrapState::NormalStart
			}
		} else {
			BootstrapState::NormalStart
		};

		// For roundup barrier, we need to count all external chains
		let roundup_barrier = Arc::new(Barrier::new(
			evm_providers
				.iter()
				.filter(|p| !p.is_native.unwrap_or(false)) // Count all external chains
				.count()
				.saturating_add(1),
		));
		let socket_bootstrap_count = Arc::new(Mutex::new(u8::default()));
		let bootstrap_state = Arc::new(RwLock::new(bootstrap_state));

		Self {
			system_providers_len,
			roundup_barrier,
			socket_bootstrap_count,
			bootstrap_state,
			bootstrap_config: bootstrap_config.clone(),
		}
	}

	pub fn dummy() -> Self {
		Self {
			system_providers_len: 0,
			roundup_barrier: Arc::new(Barrier::new(0)),
			socket_bootstrap_count: Arc::new(Default::default()),
			bootstrap_state: Arc::new(RwLock::new(BootstrapState::NormalStart)),
			bootstrap_config: None,
		}
	}
}
