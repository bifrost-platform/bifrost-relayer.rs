/// Logging and Sentry error reporting macros.
///
/// These macros provide a unified way to log messages and report them to Sentry,
/// reducing boilerplate code across the codebase.

/// Logs a message and captures it in Sentry with the specified level.
///
/// # Examples
///
/// ```ignore
/// // Error level logging with chain name (no address)
/// log_and_capture!(error, chain_name, SUB_LOG_TARGET,
///     "❗️ Failed to send transaction: {}, Error: {}", metadata, err);
///
/// // Error level logging with address
/// log_and_capture!(error, chain_name, SUB_LOG_TARGET, address,
///     "❗️ Failed to send transaction: {}, Error: {}", metadata, err);
///
/// // Warning level logging
/// log_and_capture!(warn, chain_name, SUB_LOG_TARGET,
///     "⚠️ Retrying operation: {}", reason);
///
/// // Info level logging (no Sentry capture)
/// log_and_capture!(info, chain_name, SUB_LOG_TARGET,
///     "🔖 Transaction confirmed: {}", tx_hash);
/// ```
#[macro_export]
macro_rules! log_and_capture {
	// Error level with address included (4+ args before format string)
	(error, $chain_name:expr, $sub_target:expr, $address:expr, $fmt:literal $($arg:tt)*) => {{
		let log_msg = format!(
			"-[{}]-[{}] {}",
			$crate::utils::sub_display_format($sub_target),
			$address,
			format!($fmt $($arg)*)
		);
		log::error!(target: $chain_name, "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", $chain_name),
			sentry::Level::Error,
		);
	}};

	// Error level with sub_log_target formatting (no address)
	(error, $chain_name:expr, $sub_target:expr, $fmt:literal $($arg:tt)*) => {{
		let log_msg = format!(
			"-[{}] {}",
			$crate::utils::sub_display_format($sub_target),
			format!($fmt $($arg)*)
		);
		log::error!(target: $chain_name, "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", $chain_name),
			sentry::Level::Error,
		);
	}};

	// Warning level with address included
	(warn, $chain_name:expr, $sub_target:expr, $address:expr, $fmt:literal $($arg:tt)*) => {{
		let log_msg = format!(
			"-[{}]-[{}] {}",
			$crate::utils::sub_display_format($sub_target),
			$address,
			format!($fmt $($arg)*)
		);
		log::warn!(target: $chain_name, "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", $chain_name),
			sentry::Level::Warning,
		);
	}};

	// Warning level with sub_log_target formatting (no address)
	(warn, $chain_name:expr, $sub_target:expr, $fmt:literal $($arg:tt)*) => {{
		let log_msg = format!(
			"-[{}] {}",
			$crate::utils::sub_display_format($sub_target),
			format!($fmt $($arg)*)
		);
		log::warn!(target: $chain_name, "{log_msg}");
		sentry::capture_message(
			&format!("[{}]{log_msg}", $chain_name),
			sentry::Level::Warning,
		);
	}};

	// Info level - no Sentry capture
	(info, $chain_name:expr, $sub_target:expr, $fmt:literal $($arg:tt)*) => {{
		log::info!(
			target: $chain_name,
			"-[{}] {}",
			$crate::utils::sub_display_format($sub_target),
			format!($fmt $($arg)*)
		);
	}};
}

/// Logs a simple message and captures it in Sentry (without sub_display_format).
///
/// Used for simpler logging scenarios where the sub_log_target formatting is not needed.
#[macro_export]
macro_rules! log_and_capture_simple {
	(error, $($arg:tt)*) => {{
		let log_msg = format!($($arg)*);
		log::error!("{log_msg}");
		sentry::capture_message(&log_msg, sentry::Level::Error);
	}};

	(warn, $($arg:tt)*) => {{
		let log_msg = format!($($arg)*);
		log::warn!("{log_msg}");
		sentry::capture_message(&log_msg, sentry::Level::Warning);
	}};
}
