use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
/// The response from the fee rate API. (=mempool.space)
pub struct FeeRateResponse {
	#[serde(rename = "fastestFee")]
	pub fastest_fee: u64,
	#[serde(rename = "economyFee")]
	pub economy_fee: u64,
	#[serde(rename = "minimumFee")]
	pub minimum_fee: u64,
}

/// Mempool.space endpoint. (=block height)
pub const MEMPOOL_SPACE_BLOCK_HEIGHT_ENDPOINT: &str = "https://mempool.space/api/blocks/tip/height";

/// Mempool.space testnet endpoint. (=block height)
pub const MEMPOOL_SPACE_TESTNET_BLOCK_HEIGHT_ENDPOINT: &str =
	"https://mempool.space/testnet/api/blocks/tip/height";

/// Mempool.space fee rate multiplier.
pub const MEMPOOL_SPACE_FEE_RATE_MULTIPLIER: f64 = 1.2;

/// Mempool.space endpoint. (=fee rate)
pub const MEMPOOL_SPACE_FEE_RATE_ENDPOINT: &str = "https://mempool.space/api/v1/fees/recommended";

/// Mempool.space testnet endpoint. (=fee rate)
pub const MEMPOOL_SPACE_TESTNET_FEE_RATE_ENDPOINT: &str =
	"https://mempool.space/testnet/api/v1/fees/recommended";
