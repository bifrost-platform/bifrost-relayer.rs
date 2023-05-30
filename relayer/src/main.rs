mod cli;
mod service;

use std::io::Write;

use cc_cli::{
	create_configuration,
	runner::{build_runtime, Runner},
};
use chrono::Local;
use env_logger::fmt::Color;
use log::Level;

use crate::cli::Cli;

fn main() {
	env_logger::Builder::new()
		.format(|buf, record| {
			let mut level_style = buf.style();
			let color = match record.level() {
				Level::Info => Color::Green,
				_ => Color::Red,
			};

			level_style.set_color(color).set_bold(true);

			writeln!(
				buf,
				"{} {:05} {:015}]{}",
				Local::now().format("%Y-%m-%dT%H:%M:%S"),
				level_style.value(record.level()),
				record.target(),
				record.args(),
			)
		})
		.filter(None, log::LevelFilter::Info)
		.init();

	let cli = Cli::from_args();

	let tokio_runtime = build_runtime().unwrap();
	let configuration =
		create_configuration(tokio_runtime.handle().clone(), cli.chain.clone()).unwrap();

	cli.print_relayer_infos();

	let runner = Runner::new(configuration, tokio_runtime).unwrap();
	runner
		.run_relayer_until_exit(|config| async move {
			service::relay(config).map_err(sc_cli::Error::Service)
		})
		.unwrap_or_default();
}
