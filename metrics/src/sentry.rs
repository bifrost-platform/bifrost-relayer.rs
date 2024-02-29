use sentry::ClientInitGuard;
use std::borrow::Cow;

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
				environment,
				..Default::default()
			},
		));
		return Some(sentry_client);
	}
	None
}
