/// The schedule definition for oracle price feeding. This will trigger on every 5th minute.
pub const PRICE_FEEDER_SCHEDULE: &str = "0 */5 * * * * *";

/// The schedule definition for roundup emissions. This will trigger on every 15th second.
pub const ROUNDUP_EMITTER_SCHEDULE: &str = "*/15 * * * * * *";

/// The schedule definition for heartbeats. This will trigger on every minute.
pub const HEARTBEAT_SCHEDULE: &str = "0 * * * * * *";

/// The schedule definition for rollback checks. This will trigger on every minute.
pub const ROLLBACK_CHECK_SCHEDULE: &str = "0 * * * * * *";

/// The minimum interval that should be passed in order to handle rollback checks. (=3 minutes)
pub const ROLLBACK_CHECK_MINIMUM_INTERVAL: u32 = 3 * 60;
