use cccp_primitives::eth::CLIENT_NAME_MAX_LENGTH;
use chrono::{Datelike, Local};
use clap::Parser;

#[derive(Debug, Parser)]
pub struct Cli {
	#[clap(flatten)]
	pub run: RunCmd,
}

impl Cli {
	pub fn from_args() -> Self
	where
		Self: Parser + Sized,
	{
		Self::parse_from(&mut std::env::args_os())
	}

	fn impl_name() -> String {
		"BIFROST Relayer".into()
	}

	fn impl_version() -> String {
		env!("SUBSTRATE_CLI_IMPL_VERSION").into()
	}

	fn author() -> String {
		env!("CARGO_PKG_AUTHORS").into()
	}

	fn copyright_start_year() -> i32 {
		2023
	}

	/// Log information about the relayer itself.
	pub fn print_relayer_infos(&self) {
		let mut target = String::from("cccp-relayer");
		let space = " ".repeat(CLIENT_NAME_MAX_LENGTH - target.len());
		target.push_str(&space);

		log::info!(target: &target, "{}", Self::impl_name());
		log::info!(target: &target, "✌️  version {}", Self::impl_version());
		log::info!(
			target: &target,
			"❤️  by {}, {}-{}",
			Self::author(),
			Self::copyright_start_year(),
			Local::now().year()
		);
	}
}

#[derive(Debug, Clone, Parser)]
/// The `run` command used to run a relayer.
pub struct RunCmd {
	#[arg(long)]
	pub enable_external: bool,
}
