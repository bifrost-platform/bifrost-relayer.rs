use cc_cli::Configuration;
use sc_service::{Error as ServiceError, TaskManager};
use web3::transports::Http;

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let task_manager = TaskManager::new(config.tokio_handle.clone(), None)?;

	let eth_client = cccp_client::eth::EthClient::<Http>::new(&config.private_config.eth_provider);
	let eth_event_detector = cccp_client::eth::EventDetector::new(eth_client);

	task_manager
		.spawn_essential_handle()
		.spawn_blocking("eth-event-detector", None, async move { eth_event_detector.run().await });

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
