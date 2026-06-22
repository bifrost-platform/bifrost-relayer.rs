use super::*;
use br_client::eth::handlers::{SocketOnflightHandler, SocketOnflightSender, SocketQueuePoller};
use br_client::sol::{client::SolClient, handlers::outbound::SolOutboundSender};
use tokio::sync::mpsc;

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
	/// The `SocketOnflightHandler` (single instance for Bifrost chain).
	pub socket_onflight_handler: Option<SocketOnflightHandler<F, P, N>>,
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
		mut bootstrap_shared_data: BootstrapSharedData,
		bfc_client: Arc<EthClient<F, P, N>>,
		rollback_senders: Arc<BTreeMap<ChainId, Arc<UnboundedSender<Socket_Message>>>>,
		sol_clients: Arc<BTreeMap<ChainId, SolClient>>,
		sol_outbound_senders: Arc<BTreeMap<ChainId, SolOutboundSender>>,
		task_manager: &TaskManager,
		debug_mode: bool,
	) -> Self {
		// Probe the live runtime metadata to determine if CCCPRelayQueue pallet is active.
		// OnlineClient fetches this metadata from the connected node at startup, so this
		// reflects the actual runtime — not the compile-time .scale snapshot.
		let cccp_relay_queue_enabled =
			substrate_deps.sub_client.metadata().pallet_by_name("CCCPRelayQueue").is_some();

		bootstrap_shared_data.cccp_relay_queue_enabled = cccp_relay_queue_enabled;

		if cccp_relay_queue_enabled {
			log::info!("CCCPRelayQueue pallet detected — relay queue mode enabled");
		} else {
			log::info!(
				"CCCPRelayQueue pallet not found in runtime — running in legacy socket relay mode"
			);
		}

		let mut socket_relay_handlers = vec![];
		let mut socket_queue_pollers = vec![];
		let mut roundup_relay_handlers = vec![];
		let mut onflight_senders: BTreeMap<ChainId, SocketOnflightSender> = BTreeMap::new();
		let ManagerDeps { bifrost_client, clients, event_managers } = manager_deps;

		for handler_config in config.relayer_config.handler_configs.iter() {
			match handler_config.handler_type {
				HandlerType::Socket => {
					for target in handler_config.watch_list.iter() {
						// Only create the onflight channel when the relay queue pallet is active.
						// When None, SocketRelayHandler falls back to a never-resolving future in
						// its select! arm so the channel slot never triggers a hot loop.
						let onflight_receiver = if cccp_relay_queue_enabled {
							let (tx, rx) = mpsc::unbounded_channel();
							onflight_senders.insert(*target, tx);
							Some(rx)
						} else {
							None
						};

						let socket_relay_handler = SocketRelayHandler::new(
							*target,
							event_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
							onflight_receiver,
							clients.clone(),
							bifrost_client.clone(),
							substrate_deps.xt_request_sender.clone(),
							rollback_senders.clone(),
							sol_outbound_senders.clone(),
							task_manager.spawn_handle(),
							Arc::new(bootstrap_shared_data.clone()),
							debug_mode,
							substrate_deps.sub_client.clone(),
						);
						socket_relay_handlers.push(socket_relay_handler);

						// SocketQueuePoller submits on_flight_poll / finalize_poll to the pallet.
						// Skip entirely when the pallet is absent.
						if cccp_relay_queue_enabled {
							let socket_queue_poller = SocketQueuePoller::new(
								clients.get(target).expect(INVALID_CHAIN_ID).clone(),
								bifrost_client.clone(),
								substrate_deps.sub_client.clone(),
								substrate_deps.sub_rpc_url.clone(),
								substrate_deps.xt_request_sender.clone(),
								event_managers
									.get(target)
									.expect(INVALID_CHAIN_ID)
									.sender
									.subscribe(),
								Arc::new(bootstrap_shared_data.clone()),
							)
							.await
							.expect("Failed to create SocketQueuePoller");
							socket_queue_pollers.push(socket_queue_poller);
						}
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
						sol_clients.clone(),
						sol_outbound_senders.clone(),
						Arc::new(bootstrap_shared_data.clone()),
						task_manager.spawn_handle(),
						debug_mode,
					));
				},
			}
		}

		// SocketOnflightHandler watches OnFlightTransfers storage on the Substrate chain.
		// Only create it when the relay queue pallet is present and there are socket handlers.
		let socket_onflight_handler = if cccp_relay_queue_enabled && !onflight_senders.is_empty() {
			match SocketOnflightHandler::new(
				bifrost_client.clone(),
				clients.clone(),
				sol_clients.clone(),
				substrate_deps.sub_client.clone(),
				substrate_deps.sub_rpc_url.clone(),
				Arc::new(onflight_senders),
				Arc::new(bootstrap_shared_data.clone()),
			)
			.await
			{
				Ok(handler) => Some(handler),
				Err(e) => {
					log::error!("Failed to create SocketOnflightHandler: {:?}", e);
					None
				},
			}
		} else {
			None
		};

		Self {
			socket_relay_handlers,
			socket_queue_pollers,
			roundup_relay_handlers,
			socket_onflight_handler,
		}
	}
}
