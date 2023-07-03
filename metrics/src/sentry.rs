use br_primitives::cli::SentryConfig;
use sentry::ClientInitGuard;

/// Builds a sentry client only when the sentry config exists.
pub fn build_sentry_client(
	id: String,
	sentry_config: Option<SentryConfig>,
) -> Option<ClientInitGuard> {
	if let Some(sentry_config) = sentry_config {
		if sentry_config.is_enabled && !sentry_config.dsn.is_empty() {
			let sentry_client = sentry::init((
				sentry_config.dsn,
				sentry::ClientOptions {
					release: sentry::release_name!(),
					environment: Some(id.into()),
					..Default::default()
				},
			));
			return Some(sentry_client)
		}
	}
	None
}
