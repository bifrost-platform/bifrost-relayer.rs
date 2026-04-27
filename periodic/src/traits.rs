use cron::Schedule;
use eyre::Result;
use std::collections::BTreeMap;
use std::time::Duration;
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
		let now = chrono::Utc::now();
		let Some(next_fire) = self.schedule().upcoming(chrono::Utc).next() else {
			log::error!("periodic schedule has no next tick; sleep 60s and retry");
			sleep(Duration::from_secs(60)).await;
			return;
		};

		let sleep_chrono = next_fire - now;
		let sleep_std = match sleep_chrono.to_std() {
			Ok(d) => d,
			Err(_) => {
				log::warn!(
					"next tick {:?} is not after now {:?}, sleep 1s and retry",
					next_fire,
					now
				);
				Duration::from_secs(1)
			},
		};
		sleep(sleep_std).await;
	}
}

#[async_trait::async_trait]
pub trait PriceFetcher {
	/// Get price with ticker symbol.
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse>;

	/// Get all prices of support coin/token.
	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>>;
}
