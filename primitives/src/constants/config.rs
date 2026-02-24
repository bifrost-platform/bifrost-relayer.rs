/// The native chain's average block time in seconds.
pub const NATIVE_BLOCK_TIME: u64 = 3;

/// Ethereum network's average block time in seconds.
pub const ETHEREUM_BLOCK_TIME: u64 = 12;

/// The block range chunk size for getLogs requests.
pub const BOOTSTRAP_BLOCK_CHUNK_SIZE: u64 = 2000;

/// The block offset used to measure the average block time at bootstrap.
pub const BOOTSTRAP_BLOCK_OFFSET: u64 = 100;

/// The maximum allowed staleness duration (in seconds) for volatile Chainlink price feeds
/// (e.g. BTC, WBTC). Chainlink heartbeat for these feeds is typically ~1 hour.
pub const CHAINLINK_STALENESS_THRESHOLD_VOLATILE: u64 = 3600;

/// The maximum allowed staleness duration (in seconds) for stable Chainlink price feeds
/// (e.g. USDC, USDT, DAI, JPYC, CBBTC). Chainlink heartbeat for these feeds is typically ~24 hours.
pub const CHAINLINK_STALENESS_THRESHOLD_STABLE: u64 = 86400;

/// The HTTP request timeout (in seconds) for external price source API calls.
pub const PRICE_FETCHER_REQUEST_TIMEOUT: u64 = 30;

/// The default price deviation threshold in basis points (bps).
/// 200 bps = 2%. If the market price deviates from the on-chain oracle price
/// by more than this threshold, an immediate price feed will be triggered.
pub const DEFAULT_PRICE_DEVIATION_THRESHOLD_BPS: u64 = 200;
