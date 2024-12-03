use cron::Schedule;
use eyre::Result;
use std::collections::BTreeMap;
use tokio::time::sleep;

use br_primitives::periodic::PriceResponse;

#[async_trait::async_trait]
pub trait PeriodicWorker {
	/// Returns the schedule definition.
	fn schedule(&self) -> Schedule;

	/// Starts the periodic worker.
	async fn run(&mut self) -> Result<()>;

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

#[async_trait::async_trait]
pub trait PriceFetcher {
	/// Get price with ticker symbol.
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse>;

	/// Get all prices of support coin/token.
	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>>;
}
