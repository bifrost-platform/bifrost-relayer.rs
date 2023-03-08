use crate::{Configuration, Result};
use futures::{future, future::FutureExt, pin_mut, select, Future};
use sc_service::{Error as ServiceError, TaskManager};
use sc_utils::metrics::{TOKIO_THREADS_ALIVE, TOKIO_THREADS_TOTAL};
use std::time::Duration;

#[cfg(target_family = "unix")]
async fn main<F, E>(func: F) -> std::result::Result<(), E>
where
	F: Future<Output = std::result::Result<(), E>> + future::FusedFuture,
	E: std::error::Error + Send + Sync + 'static + From<ServiceError>,
{
	use tokio::signal::unix::{signal, SignalKind};

	let mut stream_int = signal(SignalKind::interrupt()).map_err(ServiceError::Io)?;
	let mut stream_term = signal(SignalKind::terminate()).map_err(ServiceError::Io)?;

	let t1 = stream_int.recv().fuse();
	let t2 = stream_term.recv().fuse();
	let t3 = func;

	pin_mut!(t1, t2, t3);

	select! {
		_ = t1 => {},
		_ = t2 => {},
		res = t3 => res?,
	}

	Ok(())
}

/// Build a tokio runtime with all features
pub fn build_runtime() -> std::result::Result<tokio::runtime::Runtime, std::io::Error> {
	tokio::runtime::Builder::new_multi_thread()
		.on_thread_start(|| {
			TOKIO_THREADS_ALIVE.inc();
			TOKIO_THREADS_TOTAL.inc();
		})
		.on_thread_stop(|| {
			TOKIO_THREADS_ALIVE.dec();
		})
		.enable_all()
		.build()
}

/// A CCCP-Relayer CLI runtime that can be used to run a relayer
pub struct Runner {
	config: Configuration,
	tokio_runtime: tokio::runtime::Runtime,
}

impl Runner {
	pub fn new(config: Configuration, tokio_runtime: tokio::runtime::Runtime) -> Result<Runner> {
		Ok(Runner { config, tokio_runtime })
	}

	pub fn run_relayer_until_exit<F, E>(
		self,
		initialize: impl FnOnce(Configuration) -> F,
	) -> std::result::Result<(), E>
	where
		F: Future<Output = std::result::Result<TaskManager, E>>,
		E: std::error::Error + Send + Sync + 'static + From<ServiceError>,
	{
		let mut task_manager = self.tokio_runtime.block_on(initialize(self.config))?;
		let res = self.tokio_runtime.block_on(main(task_manager.future().fuse()));
		// We need to drop the task manager here to inform all tasks that they should shut down.
		//
		// This is important to be done before we instruct the tokio runtime to shutdown. Otherwise
		// the tokio runtime will wait the full 60 seconds for all tasks to stop.
		drop(task_manager);

		// Give all futures 60 seconds to shutdown, before tokio "leaks" them.
		self.tokio_runtime.shutdown_timeout(Duration::from_secs(60));

		res.map_err(Into::into)
	}
}
