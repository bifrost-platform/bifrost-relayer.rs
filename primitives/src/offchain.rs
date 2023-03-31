use async_trait::async_trait;
use ethers::types::H256;
use serde::Deserialize;
use std::{collections::HashMap, str::FromStr};

pub fn get_asset_oids() -> HashMap<String, H256> {
	HashMap::from([
		(
			"BFC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000001")
				.unwrap(),
		),
		(
			"BIFI".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000002")
				.unwrap(),
		),
		(
			"BTC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000003")
				.unwrap(),
		),
		(
			"ETH".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000004")
				.unwrap(),
		),
		(
			"BNB".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000005")
				.unwrap(),
		),
		(
			"MATIC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000006")
				.unwrap(),
		),
		(
			"AVAX".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000007")
				.unwrap(),
		),
		(
			"USDC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000008")
				.unwrap(),
		),
		(
			"BUSD".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000009")
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
