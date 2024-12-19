use super::*;

pub struct HandlerDeps<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	/// The `SocketRelayHandler`'s for each specified chain.
	pub socket_relay_handlers: Vec<SocketRelayHandler<F, P, T>>,
	/// The `RoundupRelayHandler`'s for each specified chain.
	pub roundup_relay_handlers: Vec<RoundupRelayHandler<F, P, T>>,
}

impl<F, P, T> HandlerDeps<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	pub fn new(
		config: &Configuration,
		manager_deps: &ManagerDeps<F, P, T>,
		bootstrap_shared_data: BootstrapSharedData,
		bfc_client: Arc<EthClient<F, P, T>>,
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
