/// The schedule definition for oracle price feeding. This will trigger on every 5th minute.
pub const PRICE_FEEDER_SCHEDULE: &str = "0 */5 * * * * *";

/// The schedule definition for roundup emissions. This will trigger on every 15th second.
pub const ROUNDUP_EMITTER_SCHEDULE: &str = "*/15 * * * * * *";

/// The schedule definition for public key submissions. This will trigger on every 15th second.
pub const PUB_KEY_SUBMITTER_SCHEDULE: &str = "*/15 * * * * * *";

/// The schedule definition for heartbeats. This will trigger on every minute.
pub const HEARTBEAT_SCHEDULE: &str = "0 * * * * * *";

/// The schedule definition for bitcoin rollback checks. This will trigger on 15th minute.
pub const BITCOIN_ROLLBACK_CHECK_SCHEDULE: &str = "*/15 * * * * * *";

/// The schedule definition for rollback checks. This will trigger on every minute.
pub const ROLLBACK_CHECK_SCHEDULE: &str = "0 * * * * * *";

/// The minimum interval that should be passed in order to handle rollback checks. (=3 minutes)
pub const ROLLBACK_CHECK_MINIMUM_INTERVAL: u64 = 3 * 60;

/// The schedule definition for migration detector. This will trigger on every second.
pub const MIGRATION_DETECTOR_SCHEDULE: &str = "*/1 * * * * * *";

/// The schedule definition for presubmission. This will trigger on every hour.
pub const PRESUBMISSION_SCHEDULE: &str = "0 0 * * * * *";

/// The schedule definition for PSBT signer. This will trigger on every 9th second.
pub const PSBT_SIGNER_SCHEDULE: &str = "*/9 * * * * * *";

/// The schedule definition for PSBT broadcaster. This will trigger on every 3rd second.
pub const PSBT_BROADCASTER_SCHEDULE: &str = "*/3 * * * * * *";
