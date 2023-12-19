use ethers::types::U256;
use serde::Deserialize;
use tokio::sync::mpsc::{error::SendError, UnboundedSender};

use crate::{contracts::socket::SocketMessage, eth::ChainID};

#[derive(Clone, Debug, Deserialize)]
pub struct PriceResponse {
	/// The current price of the token.
	pub price: U256,
	/// Base currency trade volume in the last 24h (for secondary sources)
	pub volume: Option<U256>,
}

#[derive(Debug, Clone, Deserialize)]
pub enum PriceSource {
	Binance,
	Chainlink,
	Coingecko,
	Gateio,
	Kucoin,
	Upbit,
}

#[derive(Clone, Debug)]
/// The channel message that contains a rollbackable socket message.
pub struct RollbackableMessage {
	/// The timestamp that this relayer has tried to process the socket event.
	/// If time passed as long as `ROLLBACK_CHECK_MINIMUM_INTERVAL`, this request will be rollbacked.
	pub timeout_started_at: U256,
	/// The rollbackable socket message. This will be either `Inbound::Requested` or `Outbound::Accepted`.
	pub socket_msg: SocketMessage,
}

impl RollbackableMessage {
	pub fn new(timestamp: U256, socket_msg: SocketMessage) -> Self {
		Self { timeout_started_at: timestamp, socket_msg }
	}
}

/// The primitive sequence ID of a single socket request.
pub type RawRequestID = u128;

/// The channel message sender for rollbackable socket messages.
pub struct RollbackSender {
	/// The unique chain ID for this sender.
	pub id: ChainID,
	/// The channel message sender.
	pub sender: UnboundedSender<SocketMessage>,
}

impl RollbackSender {
	pub fn new(id: ChainID, sender: UnboundedSender<SocketMessage>) -> Self {
		Self { id, sender }
	}

	pub fn send(&self, message: SocketMessage) -> Result<(), SendError<SocketMessage>> {
		self.sender.send(message)
	}
}

#[derive(Clone, Debug)]
/// The channel message that contains a rollbackable socket message.
pub struct RollbackableMessage {
	/// The timestamp that this relayer has tried to process the socket event.
	/// If time passed as long as `ROLLBACK_CHECK_MINIMUM_INTERVAL`, this request will be rollbacked.
	pub timeout_started_at: U256,
	/// The rollbackable socket message. This will be either `Inbound::Requested` or `Outbound::Accepted`.
	pub socket_msg: SocketMessage,
}

impl RollbackableMessage {
	pub fn new(timestamp: U256, socket_msg: SocketMessage) -> Self {
		Self { timeout_started_at: timestamp, socket_msg }
	}
}

/// The primitive sequence ID of a single socket request.
pub type RawRequestID = u128;

/// The channel message sender for rollbackable socket messages.
pub struct RollbackSender {
	/// The unique chain ID for this sender.
	pub id: ChainID,
	/// The channel message sender.
	pub sender: UnboundedSender<SocketMessage>,
}

impl RollbackSender {
	pub fn new(id: ChainID, sender: UnboundedSender<SocketMessage>) -> Self {
		Self { id, sender }
	}

	pub fn send(&self, message: SocketMessage) -> Result<(), SendError<SocketMessage>> {
		self.sender.send(message)
	}
}
