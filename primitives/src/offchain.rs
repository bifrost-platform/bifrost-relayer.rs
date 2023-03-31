use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;

pub fn get_asset_oids() -> HashMap<String, [u8; 32]> {
	HashMap::from([
		(
			"BFC".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000001")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"BIFI".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000002")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"BTC".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000003")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"ETH".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000004")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"BNB".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000005")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"MATIC".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000006")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"AVAX".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000007")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"USDC".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000008")
				.unwrap()
				.try_into()
				.unwrap(),
		),
		(
			"BUSD".to_string(),
			hex::decode("0100010000000000000000000000000000000000000000000000000000000009")
				.unwrap()
				.try_into()
				.unwrap(),
		),
	])
}

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
