use cc_cli::Configuration;
use cccp_client::eth::{EthClient, EventChannel, EventDetector, TransactionManager};
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
	let (clients, tx_managers, channels) = initialize_senders(config.clone());

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

	// Initialize & spawn event detectors
	for client in clients {
		let event_detector = EventDetector::new(client.clone(), channels.clone());

		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-event-detector", event_detector.client.get_chain_name())
					.into_boxed_str(),
			),
			Some("events"),
			async move { event_detector.run().await },
		);
	}

	Ok(RelayBase { task_manager })
}

fn initialize_senders(
	config: Configuration,
) -> (Vec<Arc<EthClient<Http>>>, Vec<TransactionManager<Http>>, Arc<Vec<EventChannel>>) {
	let mut clients = vec![];
	let mut tx_managers = vec![];
	let mut senders = vec![];

	for evm_config in config.private_config.evm_chains {
		let client = Arc::new(EthClient::new(
			Arc::new(Provider::<Http>::try_from(evm_config.provider).unwrap()),
			EthClientConfiguration {
				name: evm_config.name,
				call_interval: evm_config.interval,
				socket_address: H160::from_str(evm_config.socket.as_str()).unwrap(),
			},
		));
		let (tx_manager, channel) = TransactionManager::new(client.clone());

		clients.push(client.clone());
		tx_managers.push(tx_manager);
		senders.push(EventChannel::new(channel, evm_config.id));
	}

	(clients, tx_managers, Arc::new(senders))
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
