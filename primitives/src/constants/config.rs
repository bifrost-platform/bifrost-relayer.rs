/// The native chain's average block time in seconds.
pub const NATIVE_BLOCK_TIME: u64 = 3;

/// Ethereum network's average block time in seconds.
pub const ETHEREUM_BLOCK_TIME: u64 = 12;

/// The block range chunk size for getLogs requests.
pub const BOOTSTRAP_BLOCK_CHUNK_SIZE: u64 = 2000;

/// The block offset used to measure the average block time at bootstrap.
pub const BOOTSTRAP_BLOCK_OFFSET: u64 = 100;

/// The maximum allowed staleness duration (in seconds) for Chainlink price feeds.
/// If the `updatedAt` timestamp from `latestRoundData()` is older than this threshold,
/// the price data will be considered stale and excluded from the feeding.
/// TODO: adjust this value based on actual Chainlink heartbeat intervals per feed.
pub const CHAINLINK_STALENESS_THRESHOLD: u64 = 3600;

/// The HTTP request timeout (in seconds) for external price source API calls.
pub const PRICE_FETCHER_REQUEST_TIMEOUT: u64 = 30;
