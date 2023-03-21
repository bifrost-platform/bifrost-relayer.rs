pub mod eth;

pub type Err = Box<dyn std::error::Error + Send + Sync>;

/// The time interval used when to request a new block for BIFROST.
pub const BFC_CALL_INTERVAL_MS: u64 = 2_000;

/// The size of the queue storing blocks to prevent block reorgs for BIFROST.
pub const BFC_BLOCK_QUEUE_SIZE: u64 = 6;

/// The additional configuration details for an EVM-based chain.
pub struct EthClientConfiguration {
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// The maximum block queue size for confirmation.
	pub block_queue_size: u64,
}
