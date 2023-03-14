use web3::types::H160;

pub mod bfc {
	/// The time interval used when to request a new block for BIFROST.
	pub const BFC_CALL_INTERVAL_MS: u64 = 2_000;

	/// The size of the queue storing blocks to prevent block reorgs for BIFROST.
	pub const BFC_BLOCK_QUEUE_SIZE: u64 = 6;

	/// The socket contract address deployed on BIFROST.
	pub const BFC_SOCKET_CONTRACT_ADDRESS: &str = "0xd551F33Ca8eCb0Be83d8799D9C68a368BA36Dd52";
}

/// The socket event signature.
pub const SOCKET_EVENT_SIG: &str =
	"0x918454f530580823dd0d8cf59cacb45a6eb7cc62f222d7129efba5821e77f191";

/// The additional configuration details for an EVM-based chain.
pub struct EthClientConfiguration {
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// The maximum block queue size for confirmation.
	pub block_queue_size: u64,
	/// The socket contract address.
	pub socket_address: H160,
}
