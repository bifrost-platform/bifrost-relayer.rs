use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
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
