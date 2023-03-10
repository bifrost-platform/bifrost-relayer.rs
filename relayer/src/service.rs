use cc_cli::Configuration;
use cccp_primitives::EVMTransactionManager;
use sc_service::{Error as ServiceError, TaskManager};
use std::sync::Arc;

use ethers::providers::{Http, Provider};

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// TODO: Workers tasks should spawn from here.
	let client = Arc::new(Provider::<Http>::try_from(&config.private_config.eth_provider).unwrap());
	let mut evm_manager = EVMTransactionManager::new(client);

	task_manager.spawn_essential_handle().spawn_blocking(
		"bfc-tx-manager",
		Some("txmanager"),
		async move { evm_manager.run().await },
	);

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
