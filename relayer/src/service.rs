use cc_cli::{Configuration, HandlerType};
use cccp_client::eth::{
	BlockManager, CCCPHandler, EthClient, EventChannel, Handler, TransactionManager,
};
use cccp_primitives::eth::EthClientConfiguration;
use ethers::providers::{Http, Provider};
use sc_service::{Error as ServiceError, TaskManager};
use std::sync::Arc;

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	// initialize eth client, TransactionManager
	let (clients, tx_managers, block_managers, event_channels, block_channels) = {
		let mut clients = vec![];
		let mut tx_managers = vec![];
		let mut block_managers = vec![];
		let mut event_channels = vec![]; // producers
		let mut block_channels = vec![]; // consumers

		for evm_provider in config.relayer_config.evm_providers {
			let client = Arc::new(EthClient::new(
				Arc::new(Provider::<Http>::try_from(evm_provider.provider).unwrap()),
				EthClientConfiguration {
					name: evm_provider.name,
					id: evm_provider.id,
					call_interval: evm_provider.interval,
				},
			));
			let (tx_manager, event_channel) = TransactionManager::new(client.clone());
			let (block_manager, block_channel) = BlockManager::new(client.clone());

			clients.push(client.clone());
			tx_managers.push(tx_manager);
			block_managers.push(block_manager);
			event_channels.push(EventChannel::new(event_channel, evm_provider.id));
			block_channels.push(block_channel);
		}

		(clients, tx_managers, block_managers, Arc::new(event_channels), block_channels)
	};

	// Initialize TaskManager
	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// Spawn transaction managers' tasks
	for mut tx_manager in tx_managers {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-tx-manager", tx_manager.client.get_chain_name()).into_boxed_str(),
			),
			Some("transactions"),
			async move { tx_manager.run().await },
		)
	}

	// Initialize handlers & spawn tasks
	for handler_config in config.relayer_config.handler_configs {
		match handler_config.handler_type {
			HandlerType::Socket | HandlerType::Vault =>
				for target in handler_config.watch_list {
					let block_channel = block_channels
						.iter()
						.find(|receiver| receiver.id == target.chain_id)
						.cloned()
						.expect(&format!(
							"Unknown chain id ({:?}) required on initializing socket handler.",
							target.chain_id
						));

					let client = clients
						.iter()
						.find(|client| client.get_chain_id() == target.chain_id)
						.cloned()
						.expect(&format!(
							"Unknown chain id ({:?}) required on initializing socket handler.",
							target.chain_id
						));

					let cccp_handler = CCCPHandler::new(
						event_channels.clone(),
						block_channel.clone(),
						client.clone(),
						target.contract,
					);
					task_manager.spawn_essential_handle().spawn(
						Box::leak(
							format!(
								"{}-{}-handler",
								client.get_chain_name(),
								handler_config.handler_type.to_string(),
							)
							.into_boxed_str(),
						),
						Some("Socket"),
						async move { cccp_handler.run().await },
					);
				},
		}
	}

	// spawn block managers' tasks
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

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
