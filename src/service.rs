use cc_cli::Configuration;
use sc_service::{Error as ServiceError, TaskManager};

pub fn full_relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let task_manager = TaskManager::new(config.tokio_handle.clone(), None)?;

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
