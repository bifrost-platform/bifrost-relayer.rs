use sentry::ClientInitGuard;
use std::borrow::Cow;

use br_primitives::utils::sub_display_format;

pub const LOG_TARGET: &str = "bifrost-relayer";
const SUB_LOG_TARGET: &str = "sentry-client";

/// Builds a sentry client only when the sentry config exists.
pub fn build_sentry_client(
	is_enabled: bool,
	dsn: String,
	environment: Option<Cow<'static, str>>,
) -> Option<ClientInitGuard> {
	if is_enabled && !dsn.is_empty() {
		let sentry_client = sentry::init((
			dsn,
			sentry::ClientOptions {
				release: sentry::release_name!(),
				environment: environment.clone(),
				..Default::default()
			},
		));
		log::info!(
			target: LOG_TARGET,
			"-[{}] 🔨 Initializing sentry client with environment: {}",
			sub_display_format(SUB_LOG_TARGET),
			environment.unwrap_or_default()
		);
		return Some(sentry_client);
	}
	None
}
