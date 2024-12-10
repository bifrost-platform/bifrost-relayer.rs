use alloy::primitives::U256;
use serde::Deserialize;

use crate::contracts::socket::Socket_Struct::Socket_Message;

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
	pub timeout_started_at: u64,
	/// The rollbackable socket message. This will be either `Inbound::Requested` or `Outbound::Accepted`.
	pub socket_msg: Socket_Message,
}

impl RollbackableMessage {
	pub fn new(timestamp: u64, socket_msg: Socket_Message) -> Self {
		Self { timeout_started_at: timestamp, socket_msg }
	}
}

/// The primitive sequence ID of a single socket request.
pub type RawRequestID = u128;
