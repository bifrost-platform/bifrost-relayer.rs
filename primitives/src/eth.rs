use ethers::{
	providers::ProviderError,
	types::{Address, Signature, H160, U64},
};

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

#[derive(Clone, Copy, Debug)]
/// The roundup event status.
pub enum RoundUpEventStatus {
	NextAuthorityRelayed = 9,
	NextAuthorityCommitted,
}

impl RoundUpEventStatus {
	pub fn from_u8(status: u8) -> Self {
		match status {
			9 => RoundUpEventStatus::NextAuthorityRelayed,
			10 => RoundUpEventStatus::NextAuthorityCommitted,
			_ => panic!("Unknown roundup event status received: {:?}", status),
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
/// The socket event status.
pub enum SocketEventStatus {
	// None = 0
	Requested = 1,
	Failed,
	Executed,
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
			2 => SocketEventStatus::Failed,
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

impl From<SocketEventStatus> for u8 {
	fn from(status: SocketEventStatus) -> Self {
		match status {
			SocketEventStatus::Requested => 1,
			SocketEventStatus::Failed => 2,
			SocketEventStatus::Executed => 3,
			SocketEventStatus::Reverted => 4,
			SocketEventStatus::Accepted => 5,
			SocketEventStatus::Rejected => 6,
			SocketEventStatus::Committed => 7,
			SocketEventStatus::Rollbacked => 8,
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
	/// The number of confirmations required for a block to be processed.
	pub block_confirmations: U64,
	/// The `get_block` request interval in milliseconds.
	pub call_interval: u64,
	/// Bridge direction when bridge event points this chain as destination.
	pub if_destination_chain: BridgeDirection,
}

impl EthClientConfiguration {
	pub fn new(
		name: String,
		id: u32,
		call_interval: u64,
		block_confirmations: U64,
		if_destination_chain: BridgeDirection,
	) -> Self {
		Self { name, id, call_interval, block_confirmations, if_destination_chain }
	}
}

#[derive(Debug, PartialEq)]
pub enum BootstrapState {
	BeforeCompletion,
	AfterCompletion,
	NormalStart,
}

#[derive(Clone, Debug)]
/// The information of a recovered signature.
pub struct RecoveredSignature {
	/// The original index that represents the order from the result of `get_signatures()`.
	pub idx: usize,
	/// The signature of the message.
	pub signature: Signature,
	/// The account who signed the message.
	pub signer: Address,
}

impl RecoveredSignature {
	pub fn new(idx: usize, signature: Signature, signer: Address) -> Self {
		Self { idx, signature, signer }
	}
}
