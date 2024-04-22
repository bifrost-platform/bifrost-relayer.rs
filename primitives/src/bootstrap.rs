use std::sync::Arc;
use tokio::sync::{Barrier, Mutex, RwLock};

use crate::{
	cli::{BootstrapConfig, Configuration},
	eth::BootstrapState,
};

#[derive(Clone)]
pub struct BootstrapSharedData {
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

		let (bootstrap_states, socket_barrier_len): (BootstrapState, usize) = {
			let mut ret: (BootstrapState, usize) = (BootstrapState::NormalStart, 1);
			if let Some(bootstrap_config) = bootstrap_config.clone() {
				if bootstrap_config.is_enabled {
					ret = (BootstrapState::NodeSyncing, evm_providers.len() + 1);
				}
			}
			ret
		};

		let socket_barrier = Arc::new(Barrier::new(socket_barrier_len));
		let roundup_barrier = Arc::new(Barrier::new(
			evm_providers.iter().filter(|evm_provider| evm_provider.is_relay_target).count(),
		));
		let socket_bootstrap_count = Arc::new(Mutex::new(u8::default()));
		let roundup_bootstrap_count = Arc::new(Mutex::new(u8::default()));
		let bootstrap_states = Arc::new(RwLock::new(vec![bootstrap_states; evm_providers.len()]));

		Self {
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
			socket_barrier: Arc::new(Barrier::new(0)),
			roundup_barrier: Arc::new(Barrier::new(0)),
			socket_bootstrap_count: Arc::new(Default::default()),
			roundup_bootstrap_count: Arc::new(Default::default()),
			bootstrap_states: Arc::new(Default::default()),
			bootstrap_config: None,
		}
	}
}
