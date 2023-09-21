use std::{
	collections::BTreeMap,
	net::{Ipv4Addr, SocketAddr},
	sync::Arc,
	time::Duration,
};

use ethers::{
	providers::{Http, Provider},
	types::U64,
};
use futures::FutureExt;
use sc_service::{config::PrometheusConfig, Error as ServiceError, TaskManager};
use tokio::sync::{Barrier, Mutex, RwLock};

use br_client::eth::{
	BlockManager, BridgeRelayHandler, Eip1559TransactionManager, EthClient, EventSender, Handler,
	LegacyTransactionManager, RoundupRelayHandler, TransactionManager, WalletManager,
};
use br_periodic::{
	heartbeat_sender::HeartbeatSender, roundup_emitter::RoundupEmitter, OraclePriceFeeder,
};
use br_primitives::{
	cli::{Configuration, HandlerType, PROMETHEUS_DEFAULT_PORT},
	errors::{
		INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_PRIVATE_KEY, INVALID_PROVIDER_URL,
	},
	eth::{BootstrapState, ProviderContracts, ProviderMetadata},
	periodic::PeriodicWorker,
	sub_display_format,
};

use crate::cli::{LOG_TARGET, SUB_LOG_TARGET};

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let prometheus_config = config.relayer_config.prometheus_config;
	let bootstrap_config = config.relayer_config.bootstrap_config;
	let handler_configs = config.relayer_config.handler_configs;
	let evm_providers = config.relayer_config.evm_providers;
	let system = config.relayer_config.system;

	let (bootstrap_states, socket_barrier_len): (BootstrapState, usize) = {
		let mut ret: (BootstrapState, usize) = (BootstrapState::NormalStart, 1);
		if let Some(bootstrap_config) = bootstrap_config.clone() {
			if bootstrap_config.is_enabled {
				ret = (BootstrapState::NodeSyncing, evm_providers.len() + 1);
			}
		}
		ret
	};

	// Wait until each chain of vault/socket contract and bootstrapping is completed
	let socket_barrier = Arc::new(Barrier::new(socket_barrier_len));
	let socket_bootstrapping_count = Arc::new(Mutex::new(u8::default()));
	let bootstrap_states = Arc::new(RwLock::new(vec![bootstrap_states; evm_providers.len()]));

	let mut number_of_relay_targets: usize = 0;

	// initialize `EthClient`, `TransactionManager`, `BlockManager`
	let (clients, tx_managers, block_managers, event_channels) = {
		let mut clients = vec![];
		let mut tx_managers = (vec![], vec![]);
		let mut block_managers = BTreeMap::new();
		let mut event_senders = vec![];

		evm_providers.iter().for_each(|evm_provider| {
			let wallet =
				WalletManager::from_private_key(system.private_key.as_str(), evm_provider.id)
					.expect(INVALID_PRIVATE_KEY);

			let is_native = evm_provider.is_native.unwrap_or(false);
			let provider = Provider::<Http>::try_from(evm_provider.provider.clone())
				.expect(INVALID_PROVIDER_URL)
				.interval(Duration::from_millis(evm_provider.call_interval));
			let client = Arc::new(EthClient::new(
				wallet,
				Arc::new(provider.clone()),
				ProviderMetadata::new(
					evm_provider.name.clone(),
					evm_provider.id,
					evm_provider.block_confirmations,
					evm_provider.call_interval,
					evm_provider.get_logs_batch_size.unwrap_or(U64::from(1)),
					is_native,
				),
				ProviderContracts::new(
					Arc::new(provider),
					evm_provider.socket_address.clone(),
					evm_provider.vault_address.clone(),
					evm_provider.authority_address.clone(),
					evm_provider.relayer_manager_address.clone(),
					evm_provider.chainlink_usdc_usd_address.clone(),
					evm_provider.chainlink_usdt_usd_address.clone(),
					evm_provider.chainlink_dai_usd_address.clone(),
				),
			));

			if evm_provider.is_relay_target {
				match evm_provider.eip1559.unwrap_or(false) {
					false => {
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
						event_senders.push(Arc::new(EventSender::new(
							evm_provider.id,
							sender,
							is_native,
						)))
					},
					true => {
						let (tx_manager, sender) = Eip1559TransactionManager::new(
							client.clone(),
							system.debug_mode.unwrap_or(false),
							evm_provider.min_priority_fee.unwrap_or(u64::default()).into(),
							evm_provider.duplicate_confirm_delay,
						);
						tx_managers.1.push(tx_manager);
						event_senders.push(Arc::new(EventSender::new(
							evm_provider.id,
							sender,
							is_native,
						)))
					},
				};

				number_of_relay_targets += 1;
			}
			let block_manager = BlockManager::new(
				client.clone(),
				bootstrap_states.clone(),
				match &prometheus_config {
					Some(config) => config.is_enabled,
					None => false,
				},
			);

			clients.push(client);
			block_managers.insert(block_manager.client.get_chain_id(), block_manager);
		});

		(clients, tx_managers, block_managers, event_senders)
	};

	log::info!(
		target: LOG_TARGET,
		"-[{}] 👤 Relayer: {:?}",
		sub_display_format(SUB_LOG_TARGET),
		clients[0].address()
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

	// Initialize `TaskManager`
	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// Spawn transaction managers' tasks
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

	// initialize heartbeat sender & spawn task
	let mut heartbeat_sender = HeartbeatSender::new(
		clients
			.iter()
			.find(|client| client.metadata.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone(),
		event_channels.clone(),
	);
	task_manager
		.spawn_essential_handle()
		.spawn("heartbeat", Some("heartbeat"), async move { heartbeat_sender.run().await });

	// initialize oracle price feeder & spawn tasks
	let mut oracle_price_feeder = OraclePriceFeeder::new(event_channels.clone(), clients.clone());
	task_manager.spawn_essential_handle().spawn(
		Box::leak(
			format!("{}-oracle-price-feeder", oracle_price_feeder.client.get_chain_name())
				.into_boxed_str(),
		),
		Some("periodic"),
		async move { oracle_price_feeder.run().await },
	);

	// Initialize handlers & spawn tasks
	handler_configs.iter().for_each(|handler_config| {
		match handler_config.handler_type {
			HandlerType::BridgeRelay => handler_config.watch_list.iter().for_each(|target| {
				let mut bridge_relay_handler = BridgeRelayHandler::new(
					*target,
					event_channels.clone(),
					block_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
					clients.clone(),
					bootstrap_states.clone(),
					socket_bootstrapping_count.clone(),
					bootstrap_config.clone(),
				);

				let socket_barrier_clone = socket_barrier.clone();
				let is_bootstrapped = bootstrap_states.clone();

				task_manager.spawn_essential_handle().spawn(
					Box::leak(
						format!(
							"{}-{}-handler",
							bridge_relay_handler.client.get_chain_name(),
							handler_config.handler_type.to_string(),
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

						bridge_relay_handler.run().await
					},
				);
			}),
			HandlerType::Roundup => {
				let mut roundup_relay_handler = RoundupRelayHandler::new(
					event_channels.clone(),
					block_managers
						.get(&handler_config.watch_list[0])
						.expect(INVALID_CHAIN_ID)
						.sender
						.subscribe(),
					clients.clone(),
					socket_barrier.clone(),
					bootstrap_states.clone(),
					bootstrap_config.clone(),
					number_of_relay_targets,
				);

				task_manager.spawn_essential_handle().spawn(
					"roundup-handler",
					Some("handlers"),
					async move { roundup_relay_handler.run().await },
				);
			},
		}
	});

	// initialize roundup feeder & spawn tasks
	let mut roundup_emitter =
		RoundupEmitter::new(event_channels, clients, bootstrap_states, bootstrap_config);
	task_manager.spawn_essential_handle().spawn(
		"roundup-emitter",
		Some("roundup-emitter"),
		async move { roundup_emitter.run().await },
	);

	// spawn block managers' tasks
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

	if let Some(prometheus_config) = prometheus_config {
		if prometheus_config.is_enabled {
			let interface = match prometheus_config.is_external.unwrap_or(false) {
				true => Ipv4Addr::UNSPECIFIED,
				false => Ipv4Addr::LOCALHOST,
			};

			let prometheus = PrometheusConfig::new_with_default_registry(
				SocketAddr::new(
					interface.into(),
					prometheus_config.port.unwrap_or(PROMETHEUS_DEFAULT_PORT),
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

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the relayer.
	pub task_manager: TaskManager,
}
