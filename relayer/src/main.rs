mod service;

use cc_cli::{
	create_configuration,
	runner::{build_runtime, Runner},
};

fn main() {
	let tokio_runtime = build_runtime().unwrap();
	let configuration = create_configuration(tokio_runtime.handle().clone()).unwrap();

	let runner = Runner::new(configuration, tokio_runtime).unwrap();
	runner
		.run_relayer_until_exit(|config| async move {
			service::relay(config).map_err(sc_cli::Error::Service)
		})
		.unwrap_or_default();
}
