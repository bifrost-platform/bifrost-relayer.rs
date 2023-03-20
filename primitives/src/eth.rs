use ethers::{providers::ProviderError, types::H160};

pub type EthResult<T = ()> = Result<T, ProviderError>;

pub mod bfc_testnet {
	pub const BFC_CHAIN_ID: u32 = 49088;

	/// The time interval used when to request a new block for BIFROST.
	pub const BFC_CALL_INTERVAL_MS: u64 = 2_000;

	/// The socket contract address deployed on BIFROST.
	pub const BFC_SOCKET_CONTRACT_ADDRESS: &str = "0x0218371b18340aBD460961bdF3Bd5F01858dAB53";

	/// The vault contract address deployed on BIFROST.
	pub const BFC_VAULT_CONTRACT_ADDRESS: &str = "0x90381bB369D4F8069fdA9246b23637a78c5d1c83";
}

pub mod eth_testnet {
	pub const ETH_CHAIN_ID: u32 = 5;

	/// The time interval used when to request a new block for Ethereum.
	pub const ETH_CALL_INTERVAL_MS: u64 = 5_000;

	/// The socket contract address deployed on Ethereum.
	pub const ETH_SOCKET_CONTRACT_ADDRESS: &str = "0xeF5260Db045200142a6B5DDB297e860099ffd51d";

	/// The vault contract address deployed on Ethereum.
	pub const ETH_VAULT_CONTRACT_ADDRESS: &str = "0x7EB02c73349B3De1406e6b433c5bA1a526CBF253";
}

pub mod bsc_testnet {
	pub const BSC_CHAIN_ID: u32 = 97;

	/// The time interval used when to request a new block for Binance Smart Chain.
	pub const BSC_CALL_INTERVAL_MS: u64 = 2_000;

	/// The socket contract address deployed on Binance Smart Chain.
	pub const BSC_SOCKET_CONTRACT_ADDRESS: &str = "0x8039c3AD8ED55509fD3f6Daa78867923fDe6E61c";

	/// The vault contract address deployed on Binance Smart Chain.
	pub const BSC_VAULT_CONTRACT_ADDRESS: &str = "0x27C66cb5caa07C9B332939c357c789C606f5054C";
}

pub mod polygon_testnet {
	pub const POLYGON_CHAIN_ID: u32 = 80001;

	/// The time interval used when to request a new block for Polygon.
	pub const POLYGON_CALL_INTERVAL_MS: u64 = 1_000;

	/// The socket contract address deployed on Polygon.
	pub const POLYGON_SOCKET_CONTRACT_ADDRESS: &str = "0xA25357F3C313Bd13885678f935178211f0dF6722";

	/// The vault contract address deployed on Polygon.
	pub const POLYGON_VAULT_CONTRACT_ADDRESS: &str = "0xB2ba0020560cF6c164DC48D1E29559AbA8472208";
}

/// The socket event signature.
pub const SOCKET_EVENT_SIG: &str =
	"0x918454f530580823dd0d8cf59cacb45a6eb7cc62f222d7129efba5821e77f191";

#[derive(Clone, Copy, Debug)]
/// The socket event status.
pub enum SocketEventStatus {
	// None = 0
	Requested = 1,
	// Failed = 2
	Executed = 3,
	Reverted,
	Accepted,
	Rejected,
	Committed,
	Rollbacked,
}

impl SocketEventStatus {
	pub fn from_u8(status: u8) -> Self {
		match status {
			1 => SocketEventStatus::Requested,
			3 => SocketEventStatus::Executed,
			4 => SocketEventStatus::Reverted,
			5 => SocketEventStatus::Accepted,
			6 => SocketEventStatus::Rejected,
			7 => SocketEventStatus::Committed,
			8 => SocketEventStatus::Rollbacked,
			_ => panic!("invalid socket event status received: {:?}", status),
		}
	}
}

#[derive(Clone, Debug)]
/// The additional configuration details for an EVM-based chain.
pub struct EthClientConfiguration {
	/// The ethereum client chain ID.
	pub id: u32,
	/// The ethereum client name.
	pub name: String,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// The socket contract address.
	pub socket_address: H160,
	/// The vault contract address.
	pub vault_address: H160,
}
