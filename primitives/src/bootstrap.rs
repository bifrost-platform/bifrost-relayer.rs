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
	/// The barrier used to lock the system until the socket bootstrap process is done.
	pub socket_barrier: Arc<Barrier>,
	/// The barrier used to lock the system until the roundup bootstrap process is done.
	pub roundup_barrier: Arc<Barrier>,
	/// The current number of finished socket bootstrap processes.
	pub socket_bootstrap_count: Arc<Mutex<u8>>,
	/// The current number of finished roundup bootstrap processes.
	pub roundup_bootstrap_count: Arc<Mutex<u8>>,
	/// The current shared state of the bootstrap process.
	pub bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
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

		let (bootstrap_states, socket_barrier_len): (BootstrapState, usize) = {
			let mut ret: (BootstrapState, usize) =
				(BootstrapState::NormalStart, system_providers_len);
			if let Some(bootstrap_config) = bootstrap_config.clone() {
				if bootstrap_config.is_enabled {
					ret.0 = BootstrapState::NodeSyncing;
					ret.1 = ret.1.saturating_add(1); // add 1 for roundup handler
				}
			}
			ret
		};

		let socket_barrier = Arc::new(Barrier::new(socket_barrier_len));
		// For roundup barrier, we need:
		// 1. One for the roundup handler for each external chain
		// 2. One for the roundup emitter
		let roundup_barrier = Arc::new(Barrier::new(
			evm_providers
				.iter()
				.filter(|p| !p.is_native.unwrap_or(false)) // Count all external chains
				.count()
				.saturating_add(1), // +1 for emitter
		));
		let socket_bootstrap_count = Arc::new(Mutex::new(u8::default()));
		let roundup_bootstrap_count = Arc::new(Mutex::new(u8::default()));
		let bootstrap_states = Arc::new(RwLock::new(vec![bootstrap_states; system_providers_len]));

		Self {
			system_providers_len,
			socket_barrier,
			roundup_barrier,
			socket_bootstrap_count,
			roundup_bootstrap_count,
			bootstrap_states,
			bootstrap_config: bootstrap_config.clone(),
		}
	}

	pub fn dummy() -> Self {
		Self {
			system_providers_len: 0,
			socket_barrier: Arc::new(Barrier::new(0)),
			roundup_barrier: Arc::new(Barrier::new(0)),
			socket_bootstrap_count: Arc::new(Default::default()),
			roundup_bootstrap_count: Arc::new(Default::default()),
			bootstrap_states: Arc::new(RwLock::new(vec![BootstrapState::NormalStart])),
			bootstrap_config: None,
		}
	}
}
