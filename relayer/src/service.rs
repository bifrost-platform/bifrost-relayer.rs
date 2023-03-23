use cccp_client::eth::{
	BlockManager, CCCPHandler, EthClient, EventSender, Handler, TransactionManager, Wallet,
};
use cccp_primitives::{
	cli::{Configuration, HandlerConfig, HandlerType},
	eth::{BridgeDirection, EthClientConfiguration},
};

use ethers::{
	providers::{Http, Provider},
	types::H160,
};
use sc_service::{Error as ServiceError, TaskManager};
use std::{str::FromStr, sync::Arc};

fn get_target_contracts_by_chain_id(
	chain_id: u32,
	handler_configs: &Vec<HandlerConfig>,
) -> Vec<H160> {
	let mut target_contracts = vec![];
	for handler_config in handler_configs {
		for target in &handler_config.watch_list {
			if target.chain_id == chain_id {
				target_contracts.push(
					H160::from_str(target.contract.strip_prefix("0x").unwrap_or_default())
						.unwrap_or_default(),
				);
			}
		}
	}
	target_contracts
}

fn get_target_contract_by_chain_id_and_handler_type(
	chain_id: u32,
	handler_configs: &Vec<HandlerConfig>,
) -> H160 {
	for handler_config in handler_configs {
		match handler_config.handler_type {
			HandlerType::Socket =>
				for target in &handler_config.watch_list {
					if target.chain_id == chain_id {
						return H160::from_str(
							target.contract.strip_prefix("0x").unwrap_or_default(),
						)
						.unwrap_or_default()
					}
				},
			_ => panic!("no socket address in config"),
		}
	}
	panic!("no socket address in config")
}

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
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

			let wallet = Wallet::from_phrase_or_file(
				config.relayer_config.mnemonic.as_str(),
				evm_provider.id,
			)
			.expect("Should exist");

			let client = Arc::new(EthClient::new(
				wallet,
				Arc::new(Provider::<Http>::try_from(evm_provider.provider).unwrap()),
				EthClientConfiguration {
					name: evm_provider.name,
					id: evm_provider.id,
					call_interval: evm_provider.interval,
					if_destination_chain: match evm_provider.is_native.unwrap_or_else(|| false) {
						true => BridgeDirection::Inbound,
						_ => BridgeDirection::Outbound,
					},
					socket_address: get_target_contract_by_chain_id_and_handler_type(
						evm_provider.id,
						&config.relayer_config.handler_configs,
					),
				},
			));
			let (tx_manager, event_sender) = TransactionManager::new(client.clone());
			let block_manager = BlockManager::new(client.clone(), target_contracts);

			clients.push(client);
			tx_managers.push(tx_manager);
			block_managers.push(block_manager);
			event_senders.push(Arc::new(EventSender::new(evm_provider.id, event_sender)));
		});

		(clients, tx_managers, block_managers, event_senders)
	};

	// Initialize `TaskManager`
	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// Spawn transaction managers' tasks
	tx_managers.into_iter().for_each(|mut tx_manager| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-tx-manager", tx_manager.client.get_chain_name()).into_boxed_str(),
			),
			Some("transaction-managers"),
			async move { tx_manager.run().await },
		)
	});

	// Initialize handlers & spawn tasks
	config.relayer_config.handler_configs.iter().for_each(|handler_config| {
		match handler_config.handler_type {
			HandlerType::Socket | HandlerType::Vault =>
				handler_config.watch_list.iter().for_each(|target| {
					let block_manager = block_managers
						.iter()
						.find(|manager| manager.client.get_chain_id() == target.chain_id)
						.expect(&format!(
							"Unknown chain id ({:?}) required on initializing socket handler.",
							target.chain_id
						));
					// initialize a new block receiver
					let block_receiver = block_manager.sender.subscribe();

					let client = clients
						.iter()
						.find(|client| client.get_chain_id() == target.chain_id)
						.cloned()
						.expect(&format!(
							"Unknown chain id ({:?}) required on initializing socket handler.",
							target.chain_id
						));

					let mut cccp_handler = CCCPHandler::new(
						event_channels.clone(),
						block_receiver,
						client.clone(),
						H160::from_str(target.contract.strip_prefix("0x").unwrap_or_default())
							.unwrap_or_default(),
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
						Some("handlers"),
						async move { cccp_handler.run().await },
					);
				}),
		}
	});

	// spawn block managers' tasks
	block_managers.into_iter().for_each(|block_manager| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-tx-manager", block_manager.client.get_chain_name()).into_boxed_str(),
			),
			Some("block-managers"),
			async move { block_manager.run().await },
		)
	});

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
