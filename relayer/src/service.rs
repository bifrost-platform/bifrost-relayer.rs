use cc_cli::Configuration;
use cccp_client::eth::{
	BlockChannel, BlockManager, EthClient, EventChannel, EventHandler, SocketHandler,
	TransactionManager,
};
use cccp_primitives::eth::EthClientConfiguration;
use ethers::{
	providers::{Http, Provider},
	types::H160,
};
use sc_service::{Error as ServiceError, TaskManager};
use std::{str::FromStr, sync::Arc};

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	// initialize eth client, TransactionManager
	let (clients, tx_managers, block_managers, event_channels, block_channels) =
		initialize_senders(config.clone());

	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// spawn transaction managers
	for mut tx_manager in tx_managers {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-tx-manager", tx_manager.client.get_chain_name()).into_boxed_str(),
			),
			Some("transactions"),
			async move { tx_manager.run().await },
		)
	}

	// Initialize & spawn event handlers
	block_channels.into_iter().for_each(|c| {
		let socket_handler = SocketHandler::new(event_channels.clone(), c.clone());

		task_manager
			.spawn_essential_handle()
			.spawn("test", Some("events"), async move { socket_handler.run().await });
	});

	// spawn block managers
	for block_manager in block_managers {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-tx-manager", block_manager.client.get_chain_name()).into_boxed_str(),
			),
			Some("blocks"),
			async move { block_manager.run().await },
		)
	}

	Ok(RelayBase { task_manager })
}

fn initialize_senders(
	config: Configuration,
) -> (
	Vec<Arc<EthClient<Http>>>,
	Vec<TransactionManager<Http>>,
	Vec<BlockManager<Http>>,
	Arc<Vec<EventChannel>>,
	Vec<Arc<BlockChannel>>,
) {
	let mut clients = vec![];
	let mut tx_managers = vec![];
	let mut block_managers = vec![];
	let mut event_channels = vec![]; // producers
	let mut block_channels = vec![]; // consumers

	for evm_config in config.relayer_config.evm_providers {
		let client = Arc::new(EthClient::new(
			Arc::new(Provider::<Http>::try_from(evm_config.provider).unwrap()),
			EthClientConfiguration {
				name: evm_config.name,
				call_interval: evm_config.interval,
				socket_address: H160::from_str(evm_config.socket.as_str()).unwrap(),
			},
		));
		let (tx_manager, event_channel) = TransactionManager::new(client.clone());
		let (block_manager, block_channel) = BlockManager::new(client.clone());

		clients.push(client.clone());
		tx_managers.push(tx_manager);
		block_managers.push(block_manager);
		event_channels.push(EventChannel::new(event_channel, evm_config.id));
		block_channels.push(block_channel);
	}

	(clients, tx_managers, block_managers, Arc::new(event_channels), block_channels)
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
