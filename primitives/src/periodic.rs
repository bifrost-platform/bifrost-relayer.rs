use std::{collections::BTreeMap, fmt::Error};

use async_trait::async_trait;
use cron::Schedule;
use ethers::types::U256;
use serde::Deserialize;
use tokio::time::sleep;

pub const HEARTBEAT_SCHEDULE: &str = "0 */5 * * * * *"; // Every 5th minute.
pub const PRICE_FEEDER_SCHEDULE: &str = "*/15 * * * * * *"; // Every 15th second.
pub const ROUNDUP_EMITTER_SCHEDULE: &str = "0 * * * * * *"; // Every minute.

#[async_trait]
pub trait PeriodicWorker {
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
