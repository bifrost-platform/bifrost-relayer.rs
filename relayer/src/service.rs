use std::{
	collections::BTreeMap,
	net::{Ipv4Addr, SocketAddr},
	sync::Arc,
	time::Duration,
};

use ethers::providers::{Http, Provider};
use futures::FutureExt;
use sc_service::{config::PrometheusConfig, Error as ServiceError, TaskManager};

use br_client::eth::{
	BlockManager, BridgeRelayHandler, Eip1559TransactionManager, EthClient, EventSender, Handler,
	LegacyTransactionManager, RoundupRelayHandler, TransactionManager, WalletManager,
};
use br_periodic::{
	heartbeat_sender::HeartbeatSender, roundup_emitter::RoundupEmitter, OraclePriceFeeder,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	cli::{Configuration, HandlerType},
	constants::{DEFAULT_GET_LOGS_BATCH_SIZE, DEFAULT_MIN_PRIORITY_FEE, DEFAULT_PROMETHEUS_PORT},
	errors::{
		INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_PRIVATE_KEY, INVALID_PROVIDER_URL,
	},
	eth::{AggregatorContracts, BootstrapState, ChainID, ProtocolContracts, ProviderMetadata},
	periodic::PeriodicWorker,
	sub_display_format,
};

use crate::{
	cli::{LOG_TARGET, SUB_LOG_TARGET},
	verification::assert_configuration_validity,
};

/// Starts the relayer service.
pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

/// Initializes periodic components.
fn construct_periodics(
	bootstrap_shared_data: BootstrapSharedData,
	relayer_deps: &ManagerDeps,
) -> PeriodicDeps {
	let clients = &relayer_deps.clients;
	let event_senders = &relayer_deps.event_senders;

	// initialize the heartbeat sender
	let heartbeat_sender = HeartbeatSender::new(
		clients
			.iter()
			.find(|client| client.metadata.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone(),
		event_senders.clone(),
	);

	// initialize the oracle price feeder
	let oracle_price_feeder = OraclePriceFeeder::new(event_senders.clone(), clients.clone());

	// initialize the roundup emitter
	let roundup_emitter = RoundupEmitter::new(
		event_senders.clone(),
		clients.clone(),
		Arc::new(bootstrap_shared_data.clone()),
	);

	PeriodicDeps { heartbeat_sender, oracle_price_feeder, roundup_emitter }
}

/// Initializes `BridgeRelay` & `RoundUp` handlers.
fn construct_handlers(
	config: &Configuration,
	manager_deps: &ManagerDeps,
	bootstrap_shared_data: BootstrapSharedData,
) -> HandlerDeps {
	let mut handlers = (vec![], vec![]);
	let ManagerDeps { clients, tx_managers: _, block_managers, event_senders } = manager_deps;

	config.relayer_config.handler_configs.iter().for_each(|handler_config| {
		match handler_config.handler_type {
			HandlerType::BridgeRelay => handler_config.watch_list.iter().for_each(|target| {
				handlers.0.push(BridgeRelayHandler::new(
					*target,
					event_senders.clone(),
					block_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
					clients.clone(),
					Arc::new(bootstrap_shared_data.clone()),
				));
			}),
			HandlerType::Roundup => {
				handlers.1.push(RoundupRelayHandler::new(
					event_senders.clone(),
					block_managers
						.get(&handler_config.watch_list[0])
						.expect(INVALID_CHAIN_ID)
						.sender
						.subscribe(),
					clients.clone(),
					Arc::new(bootstrap_shared_data.clone()),
				));
			},
		}
	});
	HandlerDeps { bridge_relay_handlers: handlers.0, roundup_relay_handlers: handlers.1 }
}

/// Initializes the `EthClient`, `TransactionManager`, `BlockManager`, `EventSender` for each chain.
fn construct_managers(
	config: &Configuration,
	bootstrap_shared_data: BootstrapSharedData,
) -> ManagerDeps {
	let prometheus_config = &config.relayer_config.prometheus_config;
	let evm_providers = &config.relayer_config.evm_providers;
	let system = &config.relayer_config.system;

	let mut clients = vec![];
	let mut tx_managers = (vec![], vec![]);
	let mut block_managers = BTreeMap::new();
	let mut event_senders = vec![];

	// iterate each evm provider and construct inner components.
	evm_providers.iter().for_each(|evm_provider| {
		let is_native = evm_provider.is_native.unwrap_or(false);
		let provider = Provider::<Http>::try_from(evm_provider.provider.clone())
			.expect(INVALID_PROVIDER_URL)
			.interval(Duration::from_millis(evm_provider.call_interval));

		let client = Arc::new(EthClient::new(
			WalletManager::from_private_key(system.private_key.as_str(), evm_provider.id)
				.expect(INVALID_PRIVATE_KEY),
			Arc::new(provider.clone()),
			ProviderMetadata::new(
				evm_provider.name.clone(),
				evm_provider.id,
				evm_provider.block_confirmations,
				evm_provider.call_interval,
				evm_provider.get_logs_batch_size.unwrap_or(DEFAULT_GET_LOGS_BATCH_SIZE),
				is_native,
			),
			ProtocolContracts::new(
				Arc::new(provider.clone()),
				evm_provider.socket_address.clone(),
				evm_provider.vault_address.clone(),
				evm_provider.authority_address.clone(),
				evm_provider.relayer_manager_address.clone(),
			),
			AggregatorContracts::new(
				Arc::new(provider),
				evm_provider.chainlink_usdc_usd_address.clone(),
				evm_provider.chainlink_usdt_usd_address.clone(),
				evm_provider.chainlink_dai_usd_address.clone(),
			),
		));

		if evm_provider.is_relay_target {
			if evm_provider.eip1559.unwrap_or(false) {
				let (tx_manager, sender) = Eip1559TransactionManager::new(
					client.clone(),
					system.debug_mode.unwrap_or(false),
					evm_provider.min_priority_fee.unwrap_or(DEFAULT_MIN_PRIORITY_FEE).into(),
					evm_provider.duplicate_confirm_delay,
				);
				tx_managers.1.push(tx_manager);
				event_senders.push(Arc::new(EventSender::new(evm_provider.id, sender, is_native)));
			} else {
				let (tx_manager, sender) = LegacyTransactionManager::new(
					client.clone(),
					system.debug_mode.unwrap_or(false),
					evm_provider.escalate_interval,
					evm_provider.escalate_percentage,
					evm_provider.min_gas_price,
					evm_provider.is_initially_escalated.unwrap_or(false),
					evm_provider.duplicate_confirm_delay,
				);
				tx_managers.0.push(tx_manager);
				event_senders.push(Arc::new(EventSender::new(evm_provider.id, sender, is_native)));
			}
		}
		let block_manager = BlockManager::new(
			client.clone(),
			Arc::new(bootstrap_shared_data.clone()),
			match &prometheus_config {
				Some(config) => config.is_enabled,
				None => false,
			},
		);

		clients.push(client);
		block_managers.insert(block_manager.client.get_chain_id(), block_manager);
	});

	ManagerDeps { clients, tx_managers, block_managers, event_senders }
}

/// Spawn relayer service tasks by the `TaskManager`.
fn spawn_relayer_tasks(
	task_manager: TaskManager,
	deps: FullDeps,
	config: &Configuration,
) -> TaskManager {
	let prometheus_config = &config.relayer_config.prometheus_config;

	let FullDeps { bootstrap_shared_data, manager_deps, periodic_deps, handler_deps } = deps;

	let BootstrapSharedData {
		socket_barrier,
		roundup_barrier: _,
		socket_bootstrap_count: _,
		roundup_bootstrap_count: _,
		bootstrap_states,
		bootstrap_config: _,
	} = bootstrap_shared_data;
	let ManagerDeps { tx_managers, block_managers, clients: _, event_senders: _ } = manager_deps;
	let PeriodicDeps { mut heartbeat_sender, mut oracle_price_feeder, mut roundup_emitter } =
		periodic_deps;
	let HandlerDeps { bridge_relay_handlers, roundup_relay_handlers } = handler_deps;

	// spawn legacy transaction managers
	tx_managers.0.into_iter().for_each(|mut tx_manager| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-transaction-manager", tx_manager.client.get_chain_name())
					.into_boxed_str(),
			),
			Some("transaction-managers"),
			async move { tx_manager.run().await },
		)
	});
	// spawn eip1559 transaction managers
	tx_managers.1.into_iter().for_each(|mut tx_manager| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-transaction-manager", tx_manager.client.get_chain_name())
					.into_boxed_str(),
			),
			Some("transaction-managers"),
			async move { tx_manager.run().await },
		)
	});

	// spawn heartbeat sender
	task_manager
		.spawn_essential_handle()
		.spawn("heartbeat", Some("heartbeat"), async move { heartbeat_sender.run().await });

	// spawn oracle price feeder
	task_manager.spawn_essential_handle().spawn(
		Box::leak(
			format!("{}-oracle-price-feeder", oracle_price_feeder.client.get_chain_name())
				.into_boxed_str(),
		),
		Some("oracle"),
		async move { oracle_price_feeder.run().await },
	);

	// spawn bridge relay handlers
	bridge_relay_handlers.into_iter().for_each(|mut handler| {
		let socket_barrier_clone = socket_barrier.clone();
		let is_bootstrapped = bootstrap_states.clone();

		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!(
					"{}-{}-handler",
					handler.client.get_chain_name(),
					HandlerType::BridgeRelay.to_string(),
				)
				.into_boxed_str(),
			),
			Some("handlers"),
			async move {
				socket_barrier_clone.wait().await;

				// After All of barrier complete the waiting
				let mut guard = is_bootstrapped.write().await;
				if guard.iter().all(|s| *s == BootstrapState::BootstrapRoundUpPhase2) {
					for state in guard.iter_mut() {
						*state = BootstrapState::BootstrapBridgeRelay;
					}
				}
				drop(guard);

				handler.run().await
			},
		);
	});

	// spawn roundup relay handlers
	roundup_relay_handlers.into_iter().for_each(|mut handler| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!(
					"{}-{}-handler",
					handler.client.get_chain_name(),
					HandlerType::Roundup.to_string(),
				)
				.into_boxed_str(),
			),
			Some("handlers"),
			async move { handler.run().await },
		);
	});

	// spawn roundup emitter
	task_manager.spawn_essential_handle().spawn(
		"roundup-emitter",
		Some("roundup-emitter"),
		async move { roundup_emitter.run().await },
	);

	// spawn block managers
	block_managers.into_iter().for_each(|(_chain_id, mut block_manager)| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-block-manager", block_manager.client.get_chain_name()).into_boxed_str(),
			),
			Some("block-managers"),
			async move {
				block_manager.wait_provider_sync().await;

				block_manager.run().await
			},
		)
	});

	// spawn prometheus endpoint
	if let Some(prometheus_config) = prometheus_config {
		if prometheus_config.is_enabled {
			let interface = match prometheus_config.is_external.unwrap_or(false) {
				true => Ipv4Addr::UNSPECIFIED,
				false => Ipv4Addr::LOCALHOST,
			};

			let prometheus = PrometheusConfig::new_with_default_registry(
				SocketAddr::new(
					interface.into(),
					prometheus_config.port.unwrap_or(DEFAULT_PROMETHEUS_PORT),
				),
				String::default(),
			);

			br_metrics::setup(&prometheus.registry);

			// spawn prometheus
			task_manager.spawn_handle().spawn(
				"prometheus-endpoint",
				None,
				prometheus_endpoint::init_prometheus(prometheus.port, prometheus.registry)
					.map(drop),
			);
		}
	}
	task_manager
}

/// Log the configured relay targets.
fn print_relay_targets(manager_deps: &ManagerDeps) {
	let tx_managers = &manager_deps.tx_managers;

	log::info!(
		target: LOG_TARGET,
		"-[{}] 👤 Relayer: {:?}",
		sub_display_format(SUB_LOG_TARGET),
		&manager_deps.clients[0].address()
	);

	if !tx_managers.0.is_empty() {
		log::info!(
			target: LOG_TARGET,
			"-[{}] 🔨 Relay Targets (Legacy): {}",
			sub_display_format(SUB_LOG_TARGET),
			tx_managers.0
				.iter()
				.map(|tx_manager| tx_manager.client.get_chain_name())
				.collect::<Vec<String>>()
				.join(", ")
		);
	}
	if !tx_managers.1.is_empty() {
		log::info!(
			target: LOG_TARGET,
			"-[{}] 🔨 Relay Targets (EIP1559): {}",
			sub_display_format(SUB_LOG_TARGET),
			tx_managers.1
				.iter()
				.map(|tx_manager| tx_manager.client.get_chain_name())
				.collect::<Vec<String>>()
				.join(", ")
		);
	}
}

/// Builds the internal components for the relayer service and spawns asynchronous tasks.
fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	assert_configuration_validity(&config);

	let bootstrap_shared_data = BootstrapSharedData::new(&config);

	let manager_deps = construct_managers(&config, bootstrap_shared_data.clone());
	let periodic_deps = construct_periodics(bootstrap_shared_data.clone(), &manager_deps);
	let handler_deps = construct_handlers(&config, &manager_deps, bootstrap_shared_data.clone());

	print_relay_targets(&manager_deps);

	Ok(RelayBase {
		task_manager: spawn_relayer_tasks(
			TaskManager::new(config.clone().tokio_handle, None)?,
			FullDeps { bootstrap_shared_data, manager_deps, periodic_deps, handler_deps },
			&config,
		),
	})
}

struct RelayBase {
	/// The task manager of the relayer.
	pub task_manager: TaskManager,
}

struct ManagerDeps {
	/// The `EthClient`'s for each specified chain.
	pub clients: Vec<Arc<EthClient<Http>>>,
	/// The `TransactionManager`'s for each specified chain.
	pub tx_managers: (Vec<LegacyTransactionManager<Http>>, Vec<Eip1559TransactionManager<Http>>),
	/// The `BlockManager`'s for each specified chain.
	pub block_managers: BTreeMap<ChainID, BlockManager<Http>>,
	/// The `EventSender`'s for each specified chain.
	pub event_senders: Vec<Arc<EventSender>>,
}

struct PeriodicDeps {
	/// The `HeartbeatSender` used for system health checks.
	pub heartbeat_sender: HeartbeatSender<Http>,
	/// The `OraclePriceFeeder` used for price feeding.
	pub oracle_price_feeder: OraclePriceFeeder<Http>,
	/// The `RoundupEmitter` used for detecting and emitting new round updates.
	pub roundup_emitter: RoundupEmitter<Http>,
}

struct HandlerDeps {
	/// The `BridgeRelayHandler`'s for each specified chain.
	pub bridge_relay_handlers: Vec<BridgeRelayHandler<Http>>,
	/// The `RoundupRelayHandler`'s for each specified chain.
	pub roundup_relay_handlers: Vec<RoundupRelayHandler<Http>>,
}

/// The relayer client dependencies.
struct FullDeps {
	pub bootstrap_shared_data: BootstrapSharedData,
	pub manager_deps: ManagerDeps,
	pub periodic_deps: PeriodicDeps,
	pub handler_deps: HandlerDeps,
}
