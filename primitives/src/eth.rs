use ethers::providers::ProviderError;

pub type EthResult<T = ()> = Result<T, ProviderError>;

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

#[derive(Clone, Copy, Debug)]
pub enum BridgeDirection {
	/// From external network, to bifrost network
	Inbound,
	/// From bifrost network, to external network
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
