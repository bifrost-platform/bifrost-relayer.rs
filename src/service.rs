use std::str::FromStr;

use cc_cli::Configuration;
use cccp_client::eth::{EthClient, EventDetector};
use cccp_primitives::eth::{
	bfc::{BFC_BLOCK_QUEUE_SIZE, BFC_CALL_INTERVAL_MS, BFC_SOCKET_CONTRACT_ADDRESS},
	EthClientConfiguration,
};

use ethers::{
	providers::{Http, Provider},
	types::H160,
};
use sc_service::{Arc, Error as ServiceError, TaskManager};

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let task_manager = TaskManager::new(config.tokio_handle.clone(), None)?;

	// TODO: add event detectors for every evm-chain

	let bfc_client = EthClient::new(
		Arc::new(Provider::<Http>::try_from(&config.private_config.bfc_provider).unwrap()),
		EthClientConfiguration {
			call_interval: BFC_CALL_INTERVAL_MS,
			block_queue_size: BFC_BLOCK_QUEUE_SIZE,
			socket_address: H160::from_str(BFC_SOCKET_CONTRACT_ADDRESS).unwrap(),
		},
	);
	let mut bfc_event_detector = EventDetector::new(bfc_client);

	task_manager.spawn_essential_handle().spawn_blocking(
		"bfc-event-detector",
		Some("events"),
		async move { bfc_event_detector.run().await },
	);

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
