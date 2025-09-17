use super::*;

pub struct HandlerDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// The `SocketRelayHandler`'s for each specified chain.
	pub socket_relay_handlers: Vec<SocketRelayHandler<F, P, N>>,
	/// The `RoundupRelayHandler`'s for each specified chain.
	pub roundup_relay_handlers: Vec<RoundupRelayHandler<F, P, N>>,
}

impl<F, P, N: AlloyNetwork> HandlerDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub fn new(
		config: &Configuration,
		manager_deps: &ManagerDeps<F, P, N>,
		bootstrap_shared_data: BootstrapSharedData,
		bfc_client: Arc<EthClient<F, P, N>>,
		rollback_senders: Arc<BTreeMap<ChainId, Arc<UnboundedSender<Socket_Message>>>>,
		task_manager: &TaskManager,
		debug_mode: bool,
	) -> Self {
		let mut handlers = (vec![], vec![]);
		let ManagerDeps { bifrost_client, clients, event_managers } = manager_deps;

		config.relayer_config.handler_configs.iter().for_each(
			|handler_config| match handler_config.handler_type {
				HandlerType::Socket => handler_config.watch_list.iter().for_each(|target| {
					handlers.0.push(SocketRelayHandler::new(
						*target,
						event_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
						clients.clone(),
						bifrost_client.clone(),
						rollback_senders.clone(),
						task_manager.spawn_handle(),
						Arc::new(bootstrap_shared_data.clone()),
						debug_mode,
					));
				}),
				HandlerType::Roundup => {
					handlers.1.push(RoundupRelayHandler::new(
						bfc_client.clone(),
						event_managers
							.get(&handler_config.watch_list[0])
							.expect(INVALID_CHAIN_ID)
							.sender
							.subscribe(),
						clients.clone(),
						Arc::new(bootstrap_shared_data.clone()),
						task_manager.spawn_handle(),
						debug_mode,
					));
				},
			},
		);
		Self { socket_relay_handlers: handlers.0, roundup_relay_handlers: handlers.1 }
	}
}
