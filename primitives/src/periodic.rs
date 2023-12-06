use std::{collections::BTreeMap, fmt::Error};

use async_trait::async_trait;
use cron::Schedule;
use ethers::types::U256;
use serde::Deserialize;
use tokio::{
	sync::mpsc::{error::SendError, UnboundedSender},
	time::sleep,
};

use crate::{eth::ChainID, socket::SocketMessage};

#[derive(Clone, Debug)]
/// The channel message that contains a rollbackable socket message.
pub struct RollbackableMessage {
	/// The timestamp that this relayer has tried to process the socket event.
	pub timestamp: U256,
	/// The rollbackable socket message. This will be either `Inbound::Requested` or `Outbound::Accepted`.
	pub socket_msg: SocketMessage,
}

impl RollbackableMessage {
	pub fn new(timestamp: U256, socket_msg: SocketMessage) -> Self {
		Self { timestamp, socket_msg }
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

/// The schedule definition for oracle price feeding. This will trigger on every 5th minute.
pub const PRICE_FEEDER_SCHEDULE: &str = "0 */5 * * * * *";

/// The schedule definition for roundup emissions. This will trigger on every 15th second.
pub const ROUNDUP_EMITTER_SCHEDULE: &str = "*/15 * * * * * *";

/// The scedule definition for heartbeats. This will trigger on every minute.
pub const HEARTBEAT_SCHEDULE: &str = "0 * * * * * *";

/// The scedule definition for rollback checks. This will trigger on every minute.
pub const ROLLBACK_CHECK_SCHEDULE: &str = "0 * * * * * *";

/// The minimum interval that should be passed in order to handle rollback checks. (=5 minutes)
pub const ROLLBACK_CHECK_MINIMUM_INTERVAL: u32 = 5 * 60 * 1000;

#[async_trait]
pub trait PeriodicWorker {
	/// Returns the schedule definition.
	fn schedule(&self) -> Schedule;

	/// Starts the periodic worker.
	async fn run(&mut self);

	/// Wait until it reaches the next schedule.
	async fn wait_until_next_time(&self) {
		let sleep_duration =
			self.schedule().upcoming(chrono::Utc).next().unwrap() - chrono::Utc::now();

		match sleep_duration.to_std() {
			Ok(sleep_duration) => sleep(sleep_duration).await,
			Err(_) => return,
		}
	}
}

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

#[async_trait]
pub trait PriceFetcher {
	/// Get price with ticker symbol.
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse, Error>;

	/// Get all prices of support coin/token.
	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>, Error>;
}
