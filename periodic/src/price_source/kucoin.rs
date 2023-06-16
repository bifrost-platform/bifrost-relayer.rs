use ethers::utils::parse_ether;
use reqwest::Url;
use serde::Deserialize;

use cccp_primitives::{PriceFetcher, PriceResponse};

#[derive(Debug, Clone, Deserialize)]
struct Inner {
	/// Symbol
	symbol: String,
	/// Last traded price
	last: String,
	/// 24h volume, executed based on base currency
	vol: String,
}

#[derive(Debug, Clone, Deserialize)]
struct KucoinResponse {
	pub data: Inner,
}

pub struct KucoinPriceFetcher {
	base_url: Url,
	symbols: Vec<String>,
}

#[async_trait::async_trait]
impl PriceFetcher for KucoinPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> PriceResponse {
		let mut url = self.base_url.join("market/stats").unwrap();
		url.query_pairs_mut().append_pair("symbol", (symbol.clone() + "-USDT").as_str());

		let res = &self._send_request(url).await.data;

		PriceResponse {
			symbol: res.symbol.replace("-USDT", ""),
			price: parse_ether(&res.last).unwrap(),
			volume: parse_ether(&res.vol).unwrap().into(),
		}
	}

	async fn get_tickers(&self) -> Vec<PriceResponse> {
		let mut res = vec![];
		for symbol in &self.symbols {
			res.push(self.get_ticker_with_symbol(symbol.clone()).await);
		}

		res
	}
}

impl KucoinPriceFetcher {
	pub async fn new(symbols: Vec<String>) -> Self {
		Self {
			base_url: Url::parse("https://api.kucoin.com/api/v1/")
				.expect("Failed to parse KuCoin URL"),
			symbols,
		}
	}

	async fn _send_request(&self, url: Url) -> KucoinResponse {
		reqwest::get(url)
			.await
			.expect("Failed to send request to kucoin")
			.json::<KucoinResponse>()
			.await
			.expect("Failed to parse kucoin response")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let kucoin_fetcher = KucoinPriceFetcher::new(vec!["BFC".to_string()]).await;
		let res = kucoin_fetcher.get_ticker_with_symbol("BFC".to_string()).await;

		println!("{:#?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let kucoin_fetcher =
			KucoinPriceFetcher::new(vec!["BTC".to_string(), "ETH".to_string()]).await;
		let res = kucoin_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
