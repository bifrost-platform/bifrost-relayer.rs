use async_trait::async_trait;
use serde::Deserialize;

#[async_trait]
pub trait PeriodicWorker {
	/// Starts the periodic worker.
	async fn run(&mut self);
	/// Wait until it reaches the next schedule.
	async fn wait_until_next_time(&self);
}

#[derive(Clone, Debug, Deserialize)]
pub struct PriceResponse {
	/// The token symbol.
	pub symbol: String,
	/// The current price of the token.
	pub price: String,
}

#[derive(Debug, Clone, Deserialize)]
pub enum PriceSource {
	Binance,
	Coingecko,
	Gateio,
	Upbit,
}

#[async_trait]
pub trait PriceFetcher {
	/// Get price with ticker symbol.
	async fn get_price_with_symbol(&self, symbol: String) -> String;

	/// Get all prices of support coin/token.
	async fn get_price(&self) -> Vec<PriceResponse>;
}
