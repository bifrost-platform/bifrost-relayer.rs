use cc_cli::Configuration;
use cccp_client::eth::{EthClient, EventDetector};
use cccp_primitives::eth::{EthClientConfiguration, BFC_BLOCK_QUEUE_SIZE, BFC_CALL_INTERVAL_MS};

use sc_service::{Error as ServiceError, TaskManager};
use web3::transports::Http;

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let task_manager = TaskManager::new(config.tokio_handle.clone(), None)?;

	// TODO: add event detectors for every evm-chain

	let bfc_client = EthClient::<Http>::new(
		&config.private_config.bfc_provider,
		EthClientConfiguration {
			call_interval: BFC_CALL_INTERVAL_MS,
			block_queue_size: BFC_BLOCK_QUEUE_SIZE,
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
