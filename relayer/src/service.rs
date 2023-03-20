use cc_cli::Configuration;
use cccp_client::eth::{EthClient, EventChannels, EventDetector, TransactionManager};
use cccp_primitives::eth::{
	bfc_testnet::{BFC_CALL_INTERVAL_MS, BFC_SOCKET_CONTRACT_ADDRESS},
	bsc_testnet::{BSC_CALL_INTERVAL_MS, BSC_SOCKET_CONTRACT_ADDRESS},
	eth_testnet::{ETH_CALL_INTERVAL_MS, ETH_SOCKET_CONTRACT_ADDRESS},
	polygon_testnet::{POLYGON_CALL_INTERVAL_MS, POLYGON_SOCKET_CONTRACT_ADDRESS},
	EthClientConfiguration,
};
use sc_service::{Error as ServiceError, TaskManager};
use std::{str::FromStr, sync::Arc};

use ethers::{
	providers::{Http, Provider},
	types::H160,
};

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	// initialize eth client
	let bfc_client = Arc::new(EthClient::new(
		Arc::new(
			Provider::<Http>::try_from(config.get_evm_config_by_name("bfc")?.provider).unwrap(),
		),
		EthClientConfiguration {
			name: "bfc-testnet".to_string(),
			call_interval: BFC_CALL_INTERVAL_MS,
			socket_address: H160::from_str(BFC_SOCKET_CONTRACT_ADDRESS).unwrap(),
		},
	));
	let (mut bfc_tx_manager, bfc_channel) = TransactionManager::new(bfc_client.clone());

	let eth_client = Arc::new(EthClient::new(
		Arc::new(
			Provider::<Http>::try_from(config.get_evm_config_by_name("eth")?.provider).unwrap(),
		),
		EthClientConfiguration {
			name: "eth-testnet".to_string(),
			call_interval: ETH_CALL_INTERVAL_MS,
			socket_address: H160::from_str(ETH_SOCKET_CONTRACT_ADDRESS).unwrap(),
		},
	));
	let (mut eth_tx_manager, eth_channel) = TransactionManager::new(eth_client.clone());

	let bsc_client = Arc::new(EthClient::new(
		Arc::new(
			Provider::<Http>::try_from(config.get_evm_config_by_name("bsc")?.provider).unwrap(),
		),
		EthClientConfiguration {
			name: "bsc-testnet".to_string(),
			call_interval: BSC_CALL_INTERVAL_MS,
			socket_address: H160::from_str(BSC_SOCKET_CONTRACT_ADDRESS).unwrap(),
		},
	));
	let (mut bsc_tx_manager, bsc_channel) = TransactionManager::new(bsc_client.clone());

	let polygon_client = Arc::new(EthClient::new(
		Arc::new(
			Provider::<Http>::try_from(config.get_evm_config_by_name("matic")?.provider).unwrap(),
		),
		EthClientConfiguration {
			name: "polygon-testnet".to_string(),
			call_interval: POLYGON_CALL_INTERVAL_MS,
			socket_address: H160::from_str(POLYGON_SOCKET_CONTRACT_ADDRESS).unwrap(),
		},
	));
	let (mut polygon_tx_manager, polygon_channel) = TransactionManager::new(polygon_client.clone());

	// initialize cccp event channel
	let event_channels =
		Arc::new(EventChannels::new(bfc_channel, eth_channel, bsc_channel, polygon_channel));

	// initialize event detector
	let bfc_event_detector = EventDetector::new(bfc_client, event_channels.clone());
	let eth_event_detector = EventDetector::new(eth_client, event_channels.clone());
	let bsc_event_detector = EventDetector::new(bsc_client, event_channels.clone());
	let polygon_event_detector = EventDetector::new(polygon_client, event_channels);

	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// spawn transaction managers
	task_manager.spawn_essential_handle().spawn(
		"bfc-tx-manager",
		Some("transactions"),
		async move { bfc_tx_manager.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"eth-tx-manager",
		Some("transactions"),
		async move { eth_tx_manager.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"bsc-tx-manager",
		Some("transactions"),
		async move { bsc_tx_manager.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"polygon-tx-manager",
		Some("transactions"),
		async move { polygon_tx_manager.run().await },
	);

	// spawn event detectors
	task_manager
		.spawn_essential_handle()
		.spawn("bfc-event-detector", Some("events"), async move { bfc_event_detector.run().await });
	task_manager
		.spawn_essential_handle()
		.spawn("eth-event-detector", Some("events"), async move { eth_event_detector.run().await });
	task_manager
		.spawn_essential_handle()
		.spawn("bsc-event-detector", Some("events"), async move { bsc_event_detector.run().await });
	task_manager.spawn_essential_handle().spawn(
		"polygon-event-detector",
		Some("events"),
		async move { polygon_event_detector.run().await },
	);

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
