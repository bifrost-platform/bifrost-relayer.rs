pub mod cli;
pub mod config;
pub mod errors;
pub mod schedule;
pub mod tx;

pub const MEMPOOL_SPACE_API: &str = "https://mempool.space/api/v1/fees/recommended";
pub const MEMPOOL_SPACE_API_TESTNET: &str = "https://mempool.space/testnet/api/v1/fees/recommended";
