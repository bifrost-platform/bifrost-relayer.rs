use cccp_client::eth::{
	BlockManager, BridgeRelayArgs, BridgeRelayHandler, EthClient, EventSender, Handler,
	RoundupRelayHandler, TransactionManager, WalletManager,
};
use cccp_periodic::{
	heartbeat_sender::HeartbeatSender, roundup_emitter::RoundupEmitter, OraclePriceFeeder,
};
use cccp_primitives::{
	cli::{Configuration, HandlerConfig, HandlerType},
	errors::{
		INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_CONTRACT_ADDRESS,
		INVALID_PRIVATE_KEY, INVALID_PROVIDER_URL,
	},
	eth::{BootstrapState, BridgeDirection, Contract, EthClientConfiguration},
	periodic::PeriodicWorker,
	socket_bifrost::SocketBifrost,
	sub_display_format,
};

use ethers::{
	providers::{Http, Provider},
	types::H160,
};
use sc_service::{Error as ServiceError, TaskManager};
use std::{str::FromStr, sync::Arc};
use tokio::sync::{Barrier, Mutex, RwLock};

use crate::cli::{LOG_TARGET, SUB_LOG_TARGET};

/// Get the target contracts for the `BlockManager` to monitor.
fn get_target_contracts_by_chain_id(
	chain_id: u32,
	handler_configs: &Vec<HandlerConfig>,
) -> Vec<H160> {
	let mut target_contracts = vec![];
	for handler_config in handler_configs {
		for target in &handler_config.watch_list {
			let target_contract = H160::from_str(&target.contract).expect(INVALID_CONTRACT_ADDRESS);
			if target.chain_id == chain_id && !target_contracts.contains(&target_contract) {
				target_contracts.push(target_contract);
			}
		}
	}
	target_contracts
}

/// Builds socket `Contract` instances that supports CCCP.
fn build_socket_contracts(handler_configs: &Vec<HandlerConfig>) -> Vec<Contract> {
	let mut contracts = vec![];
	for handler_config in handler_configs {
		if let HandlerType::Socket = handler_config.handler_type {
			for socket in &handler_config.watch_list {
				contracts.push(Contract::new(
					socket.chain_id,
					H160::from_str(&socket.contract).expect(INVALID_CONTRACT_ADDRESS),
				));
			}
		}
	}
	contracts
}

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let (bootstrap_states, socket_barrier_len) =
		if config.relayer_config.bootstrap_config.is_enabled {
			(BootstrapState::NodeSyncing, config.relayer_config.evm_providers.len() * 2 + 1)
		} else {
			(BootstrapState::NormalStart, 1)
		};

	// Wait until each chain of vault/socket contract and bootstrapping is completed
	let socket_barrier = Arc::new(Barrier::new(socket_barrier_len));
	let socket_bootstrapping_count = Arc::new(Mutex::new(u8::default()));
	let bootstrap_states =
		Arc::new(RwLock::new(vec![bootstrap_states; config.relayer_config.evm_providers.len()]));

	let authority_address = &config
		.relayer_config
		.periodic_configs
		.as_ref()
		.unwrap()
		.roundup_emitter
		.authority_address
		.clone();

	let mut number_of_relay_targets: usize = 0;

	// initialize `EthClient`, `TransactionManager`, `BlockManager`
	let (clients, tx_managers, block_managers, event_channels) = {
		let mut clients = vec![];
		let mut tx_managers = vec![];
		let mut block_managers = vec![];
		let mut event_senders = vec![];

		config.relayer_config.evm_providers.into_iter().for_each(|evm_provider| {
			let target_contracts = get_target_contracts_by_chain_id(
				evm_provider.id,
				&config.relayer_config.handler_configs,
			);

			let wallet = WalletManager::from_private_key(
				config.relayer_config.private_key.as_str(),
				evm_provider.id,
			)
			.expect(INVALID_PRIVATE_KEY);

			let is_native = evm_provider.is_native.unwrap_or(false);
			let client = Arc::new(EthClient::new(
				wallet,
				Arc::new(
					Provider::<Http>::try_from(evm_provider.provider).expect(INVALID_PROVIDER_URL),
				),
				EthClientConfiguration::new(
					evm_provider.name,
					evm_provider.id,
					evm_provider.call_interval,
					evm_provider.block_confirmations,
					match is_native {
						true => BridgeDirection::Inbound,
						_ => BridgeDirection::Outbound,
					},
				),
				is_native,
			));

			if evm_provider.is_relay_target {
				let (tx_manager, event_sender) = TransactionManager::new(client.clone());
				tx_managers.push((tx_manager, client.get_chain_name()));
				event_senders.push(Arc::new(EventSender::new(
					evm_provider.id,
					event_sender,
					is_native,
				)));

				number_of_relay_targets = number_of_relay_targets + 1;
			}
			let block_manager =
				BlockManager::new(client.clone(), target_contracts, bootstrap_states.clone());

			clients.push(client);
			block_managers.push(block_manager);
		});

		(clients, tx_managers, block_managers, event_senders)
	};

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
			let client = clients
				.iter()
				.find(|client| client.get_chain_id() == price_feeder_config.chain_id)
				.expect(INVALID_CHAIN_ID)
				.clone();
			let channel = event_channels
				.iter()
				.find(|channel| channel.id == price_feeder_config.chain_id)
				.expect(INVALID_CHAIN_ID)
				.clone();

			let mut oracle_price_feeder =
				OraclePriceFeeder::new(channel, price_feeder_config.clone(), client.clone());
			task_manager.spawn_essential_handle().spawn(
				Box::leak(
					format!("{}-oracle-price-feeder", client.get_chain_name()).into_boxed_str(),
				),
				Some("periodic"),
				async move { oracle_price_feeder.run().await },
			);
		});

	// Initialize handlers & spawn tasks

	let socket_contracts = build_socket_contracts(&config.relayer_config.handler_configs);
	config.relayer_config.handler_configs.iter().for_each(|handler_config| {
		match handler_config.handler_type {
			HandlerType::Socket | HandlerType::Vault =>
				handler_config.watch_list.iter().for_each(|target| {
					let block_manager = block_managers
						.iter()
						.find(|manager| manager.client.get_chain_id() == target.chain_id)
						.expect(INVALID_CHAIN_ID);
					// initialize a new block receiver
					let block_receiver = block_manager.sender.subscribe();

					let client = clients
						.iter()
						.find(|client| client.get_chain_id() == target.chain_id)
						.cloned()
						.expect(INVALID_CHAIN_ID);

					let target_socket = socket_contracts
						.iter()
						.find(|socket| socket.chain_id == client.get_chain_id())
						.expect(INVALID_CHAIN_ID);

					let bridge_relay_args = BridgeRelayArgs {
						event_senders: event_channels.clone(),
						block_receiver,
						client: client.clone(),
						target_contract: H160::from_str(&target.contract)
							.expect(INVALID_CONTRACT_ADDRESS),
						target_socket: target_socket.address,
						socket_contracts: socket_contracts.clone(),
						bootstrap_states: bootstrap_states.clone(),
						bootstrapping_count: socket_bootstrapping_count.clone(),
						bootstrap_config: config.relayer_config.bootstrap_config.clone(),
						authority_address: authority_address.clone(),
						all_clients: clients.clone(),
					};

					let mut bridge_relay_handler = BridgeRelayHandler::new(bridge_relay_args);

					let socket_barrier_clone = socket_barrier.clone();
					let is_bootstrapped = bootstrap_states.clone();

					task_manager.spawn_essential_handle().spawn(
						Box::leak(
							format!(
								"{}-{}-handler",
								client.get_chain_name(),
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
				let client = clients
					.iter()
					.find(|client| client.get_chain_id() == handler_config.watch_list[0].chain_id)
					.cloned()
					.expect(INVALID_CHAIN_ID);
				let block_receiver = block_managers
					.iter()
					.find(|manager| {
						manager.client.get_chain_id() == handler_config.watch_list[0].chain_id
					})
					.expect(INVALID_CHAIN_ID)
					.sender
					.subscribe();

				let mut external_event_channels = event_channels.clone();
				external_event_channels.retain(|channel| !channel.is_native); // Remove bifrost network sender.

				// Initialize SocketBifrost instance
				let socket_bifrost = SocketBifrost::new(
					H160::from_str(&handler_config.watch_list[0].contract)
						.expect(INVALID_CONTRACT_ADDRESS),
					client.get_provider(),
				);

				let mut external_clients = clients.clone();
				external_clients.retain(|external_client| {
					// Remove bifrost network client.
					external_client.get_chain_id() != client.get_chain_id()
				});

				let mut roundup_relay_handler = RoundupRelayHandler::new(
					external_event_channels,
					block_receiver,
					client.clone(),
					external_clients,
					socket_bifrost,
					handler_config.roundup_utils.clone().unwrap(),
					socket_barrier.clone(),
					bootstrap_states.clone(),
					config.relayer_config.bootstrap_config.clone(),
					authority_address.clone(),
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
		event_channels
			.iter()
			.find(|sender| sender.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone(),
		clients
			.iter()
			.find(|client| client.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone(),
		config.relayer_config.periodic_configs.unwrap().roundup_emitter,
	);

	task_manager.spawn_essential_handle().spawn(
		"roundup-emitter",
		Some("roundup-emitter"),
		async move { roundup_emitter.run().await },
	);

	// spawn block managers' tasks
	block_managers.into_iter().for_each(|mut block_manager| {
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
