use ethers::{providers::ProviderError, types::H160};

pub type EthResult<T = ()> = Result<T, ProviderError>;

#[derive(Clone, Copy, Debug)]
/// Contract abstraction with an additional chain ID field.
pub struct Contract {
	/// The chain ID of the deployed network.
	pub chain_id: u32,
	/// The address of the contract.
	pub address: H160,
}

impl Contract {
	pub fn new(chain_id: u32, address: H160) -> Self {
		Self { chain_id, address }
	}
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
			_ => panic!("Unknown socket event status received: {:?}", status),
		}
	}
}

#[derive(Clone, Copy, Debug)]
/// The CCCP protocols bridge direction.
pub enum BridgeDirection {
	/// From external network, to bifrost network.
	Inbound,
	/// From bifrost network, to external network.
	Outbound,
}

#[derive(Clone, Debug)]
/// The additional configuration details for an EVM-based chain.
pub struct EthClientConfiguration {
	/// The name of chain which this client interact with.
	pub name: String,
	/// Id of chain which this client interact with.
	pub id: u32,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// Bridge direction when bridge event points this chain as destination.
	pub if_destination_chain: BridgeDirection,
}

const CLIENT_NAME_MAX_LENGTH: usize = 15;

impl EthClientConfiguration {
	pub fn new(
		mut name: String,
		id: u32,
		call_interval: u64,
		if_destination_chain: BridgeDirection,
	) -> Self {
		let space = " ".repeat(CLIENT_NAME_MAX_LENGTH - name.len());
		name.push_str(&space);

		Self { name, id, call_interval, if_destination_chain }
	}
}
