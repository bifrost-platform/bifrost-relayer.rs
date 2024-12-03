/// The default retries of a single json rpc request.
pub const DEFAULT_CALL_RETRIES: u32 = 3;

/// The default call retry interval in milliseconds.
pub const DEFAULT_CALL_RETRY_INTERVAL_MS: u64 = 3000;

/// The default retries of a single transaction request.
pub const DEFAULT_TX_RETRIES: u8 = 3;

/// The default transaction retry interval in milliseconds.
pub const DEFAULT_TX_RETRY_INTERVAL_MS: u64 = 3000;

/// The coefficient that will be multiplied on the max fee.
pub const MAX_FEE_COEFFICIENT: u64 = 2;

/// The coefficient that will be multiplied on the max priority fee.
pub const MAX_PRIORITY_FEE_COEFFICIENT: u64 = 2;
