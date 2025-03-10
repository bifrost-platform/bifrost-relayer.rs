use br_primitives::cli::{Configuration, Result as CliResult};

use sc_cli::Signals;
use sc_service::{Error as ServiceError, TaskManager};
use sc_utils::metrics::{TOKIO_THREADS_ALIVE, TOKIO_THREADS_TOTAL};

use futures::{future, future::FutureExt, pin_mut, select, Future};
use sentry::ClientInitGuard;
use std::time::Duration;

#[cfg(target_family = "unix")]
async fn main<F, E>(func: F) -> Result<(), E>
where
	F: Future<Output = Result<(), E>> + future::FusedFuture,
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
pub fn build_runtime() -> Result<tokio::runtime::Runtime, std::io::Error> {
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

/// A Bifrost-Relayer CLI runtime that can be used to run a relayer
pub struct Runner {
	config: Configuration,
	tokio_runtime: tokio::runtime::Runtime,
	pub sentry_client: Option<ClientInitGuard>,
}

impl Runner {
	pub fn new(config: Configuration, tokio_runtime: tokio::runtime::Runtime) -> CliResult<Runner> {
		let sentry_config = config.clone().relayer_config.sentry_config.unwrap_or_default();
		Ok(Runner {
			config,
			tokio_runtime,
			sentry_client: br_metrics::build_sentry_client(
				sentry_config.is_enabled,
				sentry_config.dsn,
				sentry_config.environment,
			),
		})
	}

	pub fn run_relayer_until_exit<F, E>(
		self,
		initialize: impl FnOnce(Configuration) -> F,
	) -> Result<(), E>
	where
		F: Future<Output = Result<TaskManager, E>>,
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

	pub fn async_run<F, E>(
		self,
		runner: impl FnOnce(Configuration) -> std::result::Result<F, E>,
	) -> std::result::Result<(), E>
	where
		F: Future<Output = std::result::Result<(), E>>,
		E: std::error::Error + Send + Sync + 'static + From<ServiceError> + From<sc_cli::Error>,
	{
		let future = runner(self.config)?;
		let signals = self.tokio_runtime.block_on(async { Signals::capture() })?;
		self.tokio_runtime.block_on(signals.run_until_signal(future.fuse()))?;
		// Drop the task manager before dropping the rest, to ensure that all futures were informed
		// about the shut down.
		// drop(task_manager);
		Ok(())
	}
}
