use ethers::{providers::ProviderError, types::H160};

pub type EthResult<T = ()> = Result<T, ProviderError>;

pub mod bfc_testnet {
	/// The time interval used when to request a new block for BIFROST.
	pub const BFC_CALL_INTERVAL_MS: u64 = 2_000;

	/// The socket contract address deployed on BIFROST.
	pub const BFC_SOCKET_CONTRACT_ADDRESS: &str = "0x0218371b18340aBD460961bdF3Bd5F01858dAB53";
}

/// The socket event signature.
pub const SOCKET_EVENT_SIG: &str =
	"0x918454f530580823dd0d8cf59cacb45a6eb7cc62f222d7129efba5821e77f191";

/// The additional configuration details for an EVM-based chain.
pub struct EthClientConfiguration {
	/// The ethereum client name.
	pub name: String,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// The socket contract address.
	pub socket_address: H160,
}
