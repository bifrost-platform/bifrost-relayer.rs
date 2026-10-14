/// The default retries of a single json rpc request.
pub const DEFAULT_CALL_RETRIES: u32 = 3;

/// The default call retry interval in milliseconds.
pub const DEFAULT_CALL_RETRY_INTERVAL_MS: u64 = 3000;

/// The timeout in milliseconds of a single json rpc request attempt on one provider.
pub const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;

/// How long (ms) requests stay on a fallback provider before the primary is tried again.
pub const PROVIDER_PROBE_PRIMARY_INTERVAL_MS: u64 = 300_000; // 5 minutes

/// How long (ms) a provider's head may stay still before it is considered stuck and rotated.
pub const PROVIDER_STALE_HEAD_TIMEOUT_MS: u64 = 180_000; // 3 minutes

/// How long (ms) a Bitcoin provider's head may stay still before it is considered stuck and
/// rotated. Long enough that a slow block (~0.25% of hours have none) only rarely trips it.
pub const PROVIDER_STALE_BTC_HEAD_TIMEOUT_MS: u64 = 3_600_000; // 1 hour

/// The timeout in milliseconds of a single Bitcoin json rpc request attempt on one provider.
pub const DEFAULT_BTC_CALL_TIMEOUT_MS: u64 = 60_000;

/// The default retries of a single transaction request.
pub const DEFAULT_TX_RETRIES: u8 = 3;

/// The default transaction retry interval in milliseconds.
pub const DEFAULT_TX_RETRY_INTERVAL_MS: u64 = 3000;

/// The default transaction timeout in milliseconds.
pub const DEFAULT_TX_TIMEOUT_MS: u64 = 60000;

/// The coefficient that will be multiplied on the max fee.
pub const MAX_FEE_COEFFICIENT: u64 = 2;

/// The coefficient that will be multiplied on the max priority fee.
pub const MAX_PRIORITY_FEE_COEFFICIENT: u64 = 2;

/// The interval in milliseconds before retrying a failed roundup relay to an external chain.
pub const ROUNDUP_RELAY_RETRY_INTERVAL_MS: u64 = 180_000; // 3 minutes

/// The interval in milliseconds before retrying a socket relay that was parked because the
/// destination chain hadn't yet synced to the message's `req_id.round_id` via roundup phase2.
pub const SOCKET_RELAY_RETRY_INTERVAL_MS: u64 = 180_000; // 3 minutes
