use ethers::{providers::ProviderError, types::H160};

pub type EthResult<T = ()> = Result<T, ProviderError>;

pub mod bfc_testnet {
	pub const BFC_CHAIN_ID: u32 = 49088;

	/// The time interval used when to request a new block for BIFROST.
	pub const BFC_CALL_INTERVAL_MS: u64 = 2_000;

	/// The socket contract address deployed on BIFROST.
	pub const BFC_SOCKET_CONTRACT_ADDRESS: &str = "0x0218371b18340aBD460961bdF3Bd5F01858dAB53";
}

pub mod eth_testnet {
	pub const ETH_CHAIN_ID: u32 = 5;

	/// The time interval used when to request a new block for Ethereum.
	pub const ETH_CALL_INTERVAL_MS: u64 = 5_000;

	/// The socket contract address deployed on Ethereum.
	pub const ETH_SOCKET_CONTRACT_ADDRESS: &str = "0xeF5260Db045200142a6B5DDB297e860099ffd51d";
}

pub mod bsc_testnet {
	pub const BSC_CHAIN_ID: u32 = 97;

	/// The time interval used when to request a new block for Binance Smart Chain.
	pub const BSC_CALL_INTERVAL_MS: u64 = 2_000;

	/// The socket contract address deployed on Binance Smart Chain.
	pub const BSC_SOCKET_CONTRACT_ADDRESS: &str = "0x8039c3AD8ED55509fD3f6Daa78867923fDe6E61c";
}

pub mod polygon_testnet {
	pub const POLYGON_CHAIN_ID: u32 = 80001;

	/// The time interval used when to request a new block for Polygon.
	pub const POLYGON_CALL_INTERVAL_MS: u64 = 1_000;

	/// The socket contract address deployed on Polygon.
	pub const POLYGON_SOCKET_CONTRACT_ADDRESS: &str = "0xA25357F3C313Bd13885678f935178211f0dF6722";
}

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
