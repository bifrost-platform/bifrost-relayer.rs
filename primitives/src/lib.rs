pub mod bootstrap;
pub mod cli;
pub mod constants;
pub mod contracts;
pub mod eth;
pub mod periodic;
pub mod tx;

pub fn sub_display_format(log_target: &str) -> String {
	format!("{:<019}", log_target)
}
