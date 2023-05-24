use ethers::types::{Address, Signature};

pub type ChainID = u32;

/// The native chain's average block time in seconds.
pub const NATIVE_BLOCK_TIME: u32 = 3u32;
/// Ethereum network's average block time in seconds.
pub const ETHEREUM_BLOCK_TIME: u64 = 12u64;
pub const BOOTSTRAP_BLOCK_CHUNK_SIZE: u64 = 2000;
pub const BOOTSTRAP_BLOCK_OFFSET: u32 = 100;

//EIP-1559 constants
pub const MAX_FEE_TOLERANCE_LIMIT: u128 = 50000000000; // 50 GWEI
pub const MAX_PRIORITY_FEE_PER_GAS: u128 = 1000000000; // 1 GWEI

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

#[derive(Clone, Debug, PartialEq)]
/// The state for bootstrapping
pub enum BootstrapState {
	/// phase 0. check if the node is in syncing
	NodeSyncing,
	/// phase 1-1. emit all pushed RoundUp event
	BootstrapRoundUpPhase1,
	/// phase 1-2. bootstrap for RoundUp event
	BootstrapRoundUpPhase2,
	/// phase 2. bootstrap for Bridge event
	BootstrapBridgeRelay,
	/// phase 3. process for latest block as normal
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
