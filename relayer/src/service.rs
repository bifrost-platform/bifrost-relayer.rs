use cc_cli::Configuration;
use cccp_client::eth::{
	EthClient, EventChannels, EventDetector, SocketMessage, TransactionManager,
};
use cccp_primitives::eth::EthClientConfiguration;
use ethers::{
	providers::{Http, Provider},
	types::H160,
};
use sc_service::{Error as ServiceError, TaskManager};
use std::{str::FromStr, sync::Arc};
use tokio::sync::mpsc::Sender;

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	// initialize eth client
	let (bfc_client, mut bfc_tx_manager, bfc_channel) =
		initialize_client_by_name(config.clone(), "bfc-testnet");
	let (eth_client, mut eth_tx_manager, eth_channel) =
		initialize_client_by_name(config.clone(), "eth-goerli");
	let (bsc_client, mut bsc_tx_manager, bsc_channel) =
		initialize_client_by_name(config.clone(), "bsc-testnet");
	let (polygon_client, mut polygon_tx_manager, polygon_channel) =
		initialize_client_by_name(config.clone(), "polygon-mumbai");

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

fn initialize_client_by_name(
	config: Configuration,
	name: &str,
) -> (Arc<EthClient<Http>>, TransactionManager<Http>, Sender<SocketMessage>) {
	let evm_config = config.get_evm_config_by_name(name).unwrap();

	let client = Arc::new(EthClient::new(
		Arc::new(Provider::<Http>::try_from(evm_config.provider).unwrap()),
		EthClientConfiguration {
			name: evm_config.name,
			call_interval: evm_config.interval,
			socket_address: H160::from_str(evm_config.socket.as_str()).unwrap(),
		},
	));
	let (tx_manager, channel) = TransactionManager::new(client.clone());

	(client, tx_manager, channel)
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
