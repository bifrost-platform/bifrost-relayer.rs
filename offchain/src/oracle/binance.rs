use cccp_primitives::offchain::{PriceFetcher, PriceResponse};
use serde_json::Value;

pub struct BinancePriceFetcher {
	base_url: reqwest::Url,
	symbols: Vec<String>,
}

#[async_trait::async_trait]
impl PriceFetcher for BinancePriceFetcher {
	fn new(symbols: Vec<String>) -> Self {
		Self { base_url: reqwest::Url::parse("https://api.binance.com/api/v3/").unwrap(), symbols }
	}

	async fn get_price_with_symbol(&self, symbol: String) -> String {
		let mut url = self.base_url.join("ticker/price").unwrap();
		url.query_pairs_mut().append_pair("symbol", symbol.as_str());

		let response: PriceResponse = reqwest::get(url).await.unwrap().json().await.unwrap();
		response.price
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		let param = serde_json::to_string(&self.symbols).unwrap();

		let mut url = self.base_url.join("ticker/price").unwrap();
		url.query_pairs_mut().append_pair("symbols", param.as_str());

		let response: Value = reqwest::get(url).await.unwrap().json().await.unwrap();
		let prices_response: Vec<PriceResponse> = serde_json::from_value(response).unwrap();

		prices_response
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let binance_fetcher = BinancePriceFetcher::new(vec!["BTCUSDT".to_string()]);
		let res = binance_fetcher.get_price_with_symbol("BTCUSDT".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher =
			BinancePriceFetcher::new(vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
		let res = binance_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
