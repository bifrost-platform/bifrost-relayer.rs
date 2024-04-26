/// The native chain's average block time in seconds.
pub const NATIVE_BLOCK_TIME: u32 = 3u32;

/// Bitcoin's average block time in seconds.
pub const BITCOIN_BLOCK_TIME: u32 = 600u32;

/// Ethereum network's average block time in seconds.
pub const ETHEREUM_BLOCK_TIME: u64 = 12u64;

/// The block range chunk size for getLogs requests.
pub const BOOTSTRAP_BLOCK_CHUNK_SIZE: u64 = 2000;

/// The block offset used to measure the average block time at bootstrap.
pub const BOOTSTRAP_BLOCK_OFFSET: u32 = 100;
