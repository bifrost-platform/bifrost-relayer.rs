use ethers::{providers::ProviderError, types::H160};

pub type EthResult<T = ()> = Result<T, ProviderError>;

/// The socket event signature.
pub const SOCKET_EVENT_SIG: &str =
	"0x918454f530580823dd0d8cf59cacb45a6eb7cc62f222d7129efba5821e77f191";

#[derive(Clone, Debug)]
/// The additional configuration details for an EVM-based chain.
pub struct EthClientConfiguration {
	/// The ethereum client name.
	pub name: String,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// The socket contract address.
	pub socket_address: H160,
}
