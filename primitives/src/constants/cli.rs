/// The default path for the keystore.
pub const DEFAULT_KEYSTORE_PATH: &str = "./keys";

/// The default round offset used on bootstrap. (=3 rounds)
pub const DEFAULT_BOOTSTRAP_ROUND_OFFSET: u64 = 3;

/// The default bootstrap offset for Bitcoin (in blocks)
pub const DEFAULT_BITCOIN_BOOTSTRAP_BLOCK_OFFSET: u32 = 3;

/// The default count required for Bitcoin block confirmations (in blocks)
pub const DEFAULT_BITCOIN_BLOCK_CONFIRMATIONS: u64 = 3;

/// The default port used for prometheus.
pub const DEFAULT_PROMETHEUS_PORT: u16 = 8000;

/// The default batch size used for `eth_getLogs()`. (=1 block)
pub const DEFAULT_GET_LOGS_BATCH_SIZE: u64 = 1;

/// The default escalate interval in seconds. (=12s)
pub const DEFAULT_ESCALATE_INTERVAL_SEC: u64 = 12;

/// The maximum call interval allowed in milliseconds. (=60s)
pub const MAX_CALL_INTERVAL_MS: u64 = 60_000;

/// The maximum block confirmations allowed. (=100 blocks)
pub const MAX_BLOCK_CONFIRMATIONS: u64 = 100;

/// The maximum escalate interval allowed in seconds. (=60s)
pub const MAX_ESCALATE_INTERVAL_SEC: u64 = 60;

/// The minimum batch size allowed for `eth_getLogs()`. (=1 block)
pub const MIN_GET_LOGS_BATCH_SIZE: u64 = 1;

/// The maximum round offset allowed for bootstrap. (=14 rounds)
pub const MAX_BOOTSTRAP_ROUND_OFFSET: u64 = 14;
