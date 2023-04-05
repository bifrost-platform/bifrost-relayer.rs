use cccp_primitives::{cli::Role, sub_display_format, target_display_format};
use chrono::{Datelike, Local};
use clap::{CommandFactory, FromArgMatches, Parser};

#[derive(Debug, Parser)]
pub struct Cli {
	#[clap(flatten)]
	pub run: RunCmd,
}

impl Cli {
	/// Helper function used to parse the command line arguments. This is the equivalent of
	/// [`clap::Parser::parse()`].
	///
	/// To allow running the node without subcommand, it also sets a few more settings:
	/// [`clap::Command::propagate_version`], [`clap::Command::args_conflicts_with_subcommands`],
	/// [`clap::Command::subcommand_negates_reqs`].
	///
	/// Creates `Self` from the command line arguments. Print the
	/// error message and quit the program in case of failure.
	pub fn from_args() -> Self
	where
		Self: Parser + Sized,
	{
		Self::from_iter(&mut std::env::args_os())
	}

	/// Helper function used to parse the command line arguments. This is the equivalent of
	/// [`clap::Parser::parse_from`].
	///
	/// To allow running the node without subcommand, it also sets a few more settings:
	/// [`clap::Command::propagate_version`], [`clap::Command::args_conflicts_with_subcommands`],
	/// [`clap::Command::subcommand_negates_reqs`].
	///
	/// Creates `Self` from any iterator over arguments.
	/// Print the error message and quit the program in case of failure.
	fn from_iter<I>(iter: I) -> Self
	where
		Self: Parser + Sized,
		I: IntoIterator,
		I::Item: Into<std::ffi::OsString> + Clone,
	{
		let app = <Self as CommandFactory>::command();

		let mut full_version = Self::impl_version();
		full_version.push('\n');

		let name = Self::executable_name();
		let author = Self::author();
		let about = Self::description();
		let app = app
			.name(name)
			.author(author)
			.about(about)
			.version(full_version)
			.propagate_version(true)
			.args_conflicts_with_subcommands(true)
			.subcommand_negates_reqs(true);

		let matches = app.try_get_matches_from(iter).unwrap_or_else(|e| e.exit());

		<Self as FromArgMatches>::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
	}

	/// Implementation name.
	fn impl_name() -> String {
		"BIFROST Relayer".into()
	}

	/// Implementation version.
	///
	/// By default this will look like this:
	///
	/// `2.0.0-b950f731c`
	///
	/// Where the hash is the short commit hash of the commit of in the Git repository.
	fn impl_version() -> String {
		env!("SUBSTRATE_CLI_IMPL_VERSION").into()
	}

	/// Executable file name.
	///
	/// Extracts the file name from `std::env::current_exe()`.
	/// Resorts to the env var `CARGO_PKG_NAME` in case of Error.
	fn executable_name() -> String {
		std::env::current_exe()
			.ok()
			.and_then(|e| e.file_name().map(|s| s.to_os_string()))
			.and_then(|w| w.into_string().ok())
			.unwrap_or_else(|| env!("CARGO_PKG_NAME").into())
	}

	/// Executable file description.
	fn description() -> String {
		env!("CARGO_PKG_DESCRIPTION").into()
	}

	/// Executable file author.
	fn author() -> String {
		env!("CARGO_PKG_AUTHORS").into()
	}

	/// Copyright starting year (x-current year)
	fn copyright_start_year() -> i32 {
		2023
	}

	/// Log information about the relayer itself.
	pub fn print_relayer_infos(&self) {
		let target = target_display_format(LOG_TARGET);
		let sub_target = sub_display_format(SUB_LOG_TARGET);

		log::info!(target: &target, "-[{}] {}", sub_target, Self::impl_name());
		log::info!(target: &target, "-[{}] ✌️  version {}", sub_target, Self::impl_version());
		log::info!(
			target: &target,
			"-[{}] ❤️  by {}, {}-{}",
			sub_target,
			Self::author(),
			Self::copyright_start_year(),
			Local::now().year()
		);

		if self.run.enable_external {
			log::info!(target: &target, "-[{}] 👤 Role: {}", sub_target, Role::External);
		} else {
			log::info!(target: &target, "-[{}] 👤 Role: {}", sub_target, Role::Native);
		}
	}
}

const LOG_TARGET: &str = "cccp-relayer";
const SUB_LOG_TARGET: &str = "main";

#[derive(Debug, Clone, Parser)]
/// The `run` command used to run a relayer.
pub struct RunCmd {
	/// Enable transaction requests to external chains. ex) Relay transactions to Ethereum.
	///
	/// If enabled, the system will verify your relayer accounts balance whether it's empty or not
	/// on initialization. By default it's disabled.
	#[arg(long)]
	pub enable_external: bool,
}
