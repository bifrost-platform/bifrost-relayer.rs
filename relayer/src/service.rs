use crate::cli::{LOG_TARGET, SUB_LOG_TARGET};
use cccp_client::eth::{
	BlockManager, BridgeRelayHandler, EthClient, EventSender, Handler, RoundupRelayHandler,
	TransactionManager, WalletManager,
};
use cccp_periodic::{
	heartbeat_sender::HeartbeatSender, roundup_emitter::RoundupEmitter, OraclePriceFeeder,
};
use cccp_primitives::{
	cli::{Configuration, HandlerType},
	errors::{
		INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_CONTRACT_ADDRESS,
		INVALID_PRIVATE_KEY, INVALID_PROVIDER_URL,
	},
	eth::BootstrapState,
	periodic::PeriodicWorker,
	sub_display_format,
};
use ethers::{
	providers::{Http, Provider},
	types::H160,
};
use sc_service::{Error as ServiceError, TaskManager};
use std::{collections::BTreeMap, str::FromStr, sync::Arc};
use tokio::sync::{Barrier, Mutex, RwLock};

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let (bootstrap_states, socket_barrier_len) =
		if config.relayer_config.bootstrap_config.is_enabled {
			(BootstrapState::NodeSyncing, config.relayer_config.evm_providers.len() + 1)
		} else {
			(BootstrapState::NormalStart, 1)
		};

	// Wait until each chain of vault/socket contract and bootstrapping is completed
	let socket_barrier = Arc::new(Barrier::new(socket_barrier_len));
	let socket_bootstrapping_count = Arc::new(Mutex::new(u8::default()));
	let bootstrap_states =
		Arc::new(RwLock::new(vec![bootstrap_states; config.relayer_config.evm_providers.len()]));

	let mut number_of_relay_targets: usize = 0;

	// initialize `EthClient`, `TransactionManager`, `BlockManager`
	let (clients, tx_managers, block_managers, event_channels) = {
		let mut clients = vec![];
		let mut tx_managers = vec![];
		let mut block_managers = BTreeMap::new();
		let mut event_senders = vec![];

		config.relayer_config.evm_providers.iter().for_each(|evm_provider| {
			let wallet = WalletManager::from_private_key(
				config.relayer_config.private_key.as_str(),
				evm_provider.id,
			)
			.expect(INVALID_PRIVATE_KEY);

			let is_native = evm_provider.is_native.unwrap_or(false);
			let client = Arc::new(EthClient::new(
				wallet,
				Arc::new(
					Provider::<Http>::try_from(evm_provider.provider.clone())
						.expect(INVALID_PROVIDER_URL),
				),
				evm_provider.name.clone(),
				evm_provider.id.clone(),
				evm_provider.block_confirmations,
				evm_provider.call_interval,
				is_native,
				evm_provider.socket_address.clone(),
				evm_provider.vault_address.clone(),
				evm_provider.authority_address.clone(),
			));

			if evm_provider.is_relay_target {
				let (tx_manager, event_sender) = TransactionManager::new(client.clone());
				tx_managers.push((tx_manager, client.get_chain_name()));
				event_senders.push(Arc::new(EventSender::new(
					evm_provider.id,
					event_sender,
					is_native,
				)));

				number_of_relay_targets += 1;
			}
			let block_manager = BlockManager::new(
				client.clone(),
				vec![
					H160::from_str(&evm_provider.vault_address).expect(INVALID_CONTRACT_ADDRESS),
					H160::from_str(&evm_provider.socket_address).expect(INVALID_CONTRACT_ADDRESS),
				],
				bootstrap_states.clone(),
			);

			clients.push(client);
			block_managers.insert(block_manager.client.get_chain_id(), block_manager);
		});

		(clients, tx_managers, block_managers, event_senders)
	};
	let relayer_manager_address = config
		.relayer_config
		.evm_providers
		.iter()
		.find(|provider| provider.is_native.unwrap_or(false))
		.expect(INVALID_BIFROST_NATIVENESS)
		.relayer_manager_address
		.clone()
		.unwrap();

	log::info!(
		target: LOG_TARGET,
		"-[{}] 👤 Relayer: {:?}",
		sub_display_format(SUB_LOG_TARGET),
		clients[0].address()
	);
	log::info!(
		target: LOG_TARGET,
		"-[{}] 🔨 Relay Targets: {}",
		sub_display_format(SUB_LOG_TARGET),
		tx_managers
			.iter()
			.map(|tx_manager| tx_manager.0.client.get_chain_name())
			.collect::<Vec<String>>()
			.join(", ")
	);

	// Initialize `TaskManager`
	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// Spawn transaction managers' tasks
	tx_managers.into_iter().for_each(|(mut tx_manager, chain_name)| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(format!("{}-transaction-manager", chain_name).into_boxed_str()),
			Some("transaction-managers"),
			async move { tx_manager.run().await },
		)
	});

	// initialize heartbeat sender & spawn task
	let mut heartbeat_sender = HeartbeatSender::new(
		config.relayer_config.periodic_configs.clone().unwrap().heartbeat,
		clients
			.iter()
			.find(|client| client.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone(),
		event_channels.clone(),
		relayer_manager_address.clone(),
	);
	task_manager
		.spawn_essential_handle()
		.spawn("heartbeat", Some("heartbeat"), async move { heartbeat_sender.run().await });

	// initialize oracle price feeder & spawn tasks
	config
		.relayer_config
		.periodic_configs
		.clone()
		.unwrap()
		.oracle_price_feeder
		.unwrap()
		.iter()
		.for_each(|price_feeder_config| {
			let mut oracle_price_feeder = OraclePriceFeeder::new(
				event_channels.clone(),
				price_feeder_config.clone(),
				clients.clone(),
			);
			task_manager.spawn_essential_handle().spawn(
				Box::leak(
					format!("{}-oracle-price-feeder", oracle_price_feeder.client.get_chain_name())
						.into_boxed_str(),
				),
				Some("periodic"),
				async move { oracle_price_feeder.run().await },
			);
		});

	// Initialize handlers & spawn tasks
	config.relayer_config.handler_configs.iter().for_each(|handler_config| {
		match handler_config.handler_type {
			HandlerType::BridgeRelay => handler_config.watch_list.iter().for_each(|target| {
				let mut bridge_relay_handler = BridgeRelayHandler::new(
					*target,
					event_channels.clone(),
					block_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
					clients.clone(),
					bootstrap_states.clone(),
					socket_bootstrapping_count.clone(),
					config.relayer_config.bootstrap_config.clone(),
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
						if guard.iter().all(|s| *s == BootstrapState::BootstrapRoundUp) {
							for state in guard.iter_mut() {
								*state = BootstrapState::BootstrapSocket;
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
					config.relayer_config.bootstrap_config.clone(),
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
	let mut roundup_emitter = RoundupEmitter::new(
		event_channels.clone(),
		clients.clone(),
		config.relayer_config.periodic_configs.unwrap().roundup_emitter,
		relayer_manager_address.clone(),
	);
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
				block_manager.is_syncing().await;

				block_manager.run().await
			},
		)
	});

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the relayer.
	pub task_manager: TaskManager,
}
