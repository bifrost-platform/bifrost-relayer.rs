use super::*;
use br_client::eth::handlers::SocketQueuePoller;

pub struct HandlerDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// The `SocketRelayHandler`'s for each specified chain.
	pub socket_relay_handlers: Vec<SocketRelayHandler<F, P, N>>,
	/// The `SocketQueuePoller`'s for each specified chain.
	pub socket_queue_pollers: Vec<SocketQueuePoller<F, P, N>>,
	/// The `RoundupRelayHandler`'s for each specified chain.
	pub roundup_relay_handlers: Vec<RoundupRelayHandler<F, P, N>>,
}

impl<F, P, N: AlloyNetwork> HandlerDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub async fn new(
		config: &Configuration,
		manager_deps: &ManagerDeps<F, P, N>,
		substrate_deps: &SubstrateDeps<F, P, N>,
		bootstrap_shared_data: BootstrapSharedData,
		bfc_client: Arc<EthClient<F, P, N>>,
		rollback_senders: Arc<BTreeMap<ChainId, Arc<UnboundedSender<Socket_Message>>>>,
		task_manager: &TaskManager,
		debug_mode: bool,
	) -> Self {
		let mut socket_relay_handlers = vec![];
		let mut socket_queue_pollers = vec![];
		let mut roundup_relay_handlers = vec![];
		let ManagerDeps { bifrost_client, clients, event_managers } = manager_deps;

		for handler_config in config.relayer_config.handler_configs.iter() {
			match handler_config.handler_type {
				HandlerType::Socket => {
					for target in handler_config.watch_list.iter() {
						// Create SocketRelayHandler with Substrate client
						let socket_relay_handler = SocketRelayHandler::new(
							*target,
							event_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
							clients.clone(),
							bifrost_client.clone(),
							substrate_deps.sub_client.clone(),
							substrate_deps.sub_rpc_url.clone(),
							substrate_deps.xt_request_sender.clone(),
							rollback_senders.clone(),
							task_manager.spawn_handle(),
							Arc::new(bootstrap_shared_data.clone()),
							debug_mode,
						)
						.await
						.expect("Failed to create SocketRelayHandler");
						socket_relay_handlers.push(socket_relay_handler);

						// Create SocketQueuePoller
						let socket_queue_poller = SocketQueuePoller::new(
							clients.get(target).expect(INVALID_CHAIN_ID).clone(),
							bifrost_client.clone(),
							substrate_deps.sub_client.clone(),
							substrate_deps.sub_rpc_url.clone(),
							substrate_deps.xt_request_sender.clone(),
							event_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
							Arc::new(bootstrap_shared_data.clone()),
						)
						.await
						.expect("Failed to create SocketQueuePoller");
						socket_queue_pollers.push(socket_queue_poller);
					}
				},
				HandlerType::Roundup => {
					roundup_relay_handlers.push(RoundupRelayHandler::new(
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
			}
		}

		Self { socket_relay_handlers, socket_queue_pollers, roundup_relay_handlers }
	}
}
