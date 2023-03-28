use ethers::types::TransactionRequest;
use serde::Deserialize;

#[async_trait::async_trait]
pub trait OffchainWorker {
	/// Starts the offchain worker. Do what it have to do according to a fixed schedule.
	async fn run(&mut self);

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(&self, dst_chain_id: u32, transaction: TransactionRequest);
}

#[async_trait::async_trait]
pub trait TimeDrivenOffchainWorker {
	/// Wait until next schedule
	async fn wait_until_next_time();
}

#[derive(Debug, Deserialize)]
pub struct PriceResponse {
	pub symbol: String,
	pub price: String,
}

#[async_trait::async_trait]
pub trait PriceFetcher<T> {
	/// Instantiates a new `PriceFetcher` instance.
	fn new(symbols: Vec<String>) -> Self;

	/// Get price with ticker symbol
	async fn get_price_with_symbol(&self, symbol: String) -> String;

	/// Get all prices of support coin/token
	async fn get_price(&self) -> Vec<PriceResponse>;

	/// Send request to price source
	async fn _send_request(&self, url: reqwest::Url) -> T ;
}
