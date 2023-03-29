use async_trait::async_trait;
use serde::Deserialize;

#[async_trait]
pub trait OffchainWorker {
	/// Starts the offchain worker.
	async fn run(&mut self);
}

#[async_trait]
pub trait TimeDrivenOffchainWorker {
	/// Wait until next schedule
	async fn wait_until_next_time(&self);
}

#[derive(Debug, Deserialize)]
pub struct PriceResponse {
	pub symbol: String,
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
	/// Get price with ticker symbol
	async fn get_price_with_symbol(&self, symbol: String) -> String;

	/// Get all prices of support coin/token
	async fn get_price(&self) -> Vec<PriceResponse>;
}
