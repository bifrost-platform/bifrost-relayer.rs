use br_client::eth::ClientMap;

use super::*;

pub struct ManagerDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Bifrost client
	pub bifrost_client: Arc<EthClient<F, P, N>>,
	/// The `EthClient`'s for each specified chain.
	pub clients: Arc<ClientMap<F, P, N>>,
	/// The `EventManager`'s for each specified chain.
	pub event_managers: BTreeMap<ChainId, EventManager<F, P, N>>,
}

impl<F, P, N: AlloyNetwork> ManagerDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Initializes the `EthClient`'s and `EventManager`'s for each chain.
	pub fn new(
		config: &Configuration,
		clients: Arc<ClientMap<F, P, N>>,
		bootstrap_shared_data: BootstrapSharedData,
	) -> Self {
		let prometheus_config = &config.relayer_config.prometheus_config;
		let evm_providers = &config.relayer_config.evm_providers;
		let mut bifrost_client = None;

		let mut event_managers = BTreeMap::new();

		// iterate each evm provider and construct inner components.
		evm_providers.iter().for_each(|evm_provider| {
			let client = clients.get(&evm_provider.id).unwrap().clone();
			if evm_provider.is_native.unwrap_or(false) {
				bifrost_client = Some(client.clone());
			}
			let event_manager = EventManager::new(
				client,
				Arc::new(bootstrap_shared_data.clone()),
				match &prometheus_config {
					Some(config) => config.is_enabled,
					None => false,
				},
			);

			event_managers.insert(event_manager.client.chain_id(), event_manager);
		});

		Self {
			bifrost_client: bifrost_client.expect(INVALID_BIFROST_NATIVENESS),
			clients,
			event_managers,
		}
	}
}
