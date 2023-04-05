pub mod cli;
pub mod contracts;
pub mod eth;
pub mod periodic;

pub use cli::{RoundupHandlerUtilType, RoundupHandlerUtilityConfig};
pub use contracts::*;
pub use periodic::*;

pub fn target_display_format(log_target: &str) -> String {
	format!("{:<019}", log_target)
}

pub fn sub_display_format(log_target: &str) -> String {
	format!("{:<015}", log_target)
}
