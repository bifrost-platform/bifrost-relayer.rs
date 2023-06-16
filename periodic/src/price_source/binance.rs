use ethers::utils::parse_ether;
use reqwest::Url;
use serde::Deserialize;

use cccp_primitives::periodic::{PriceFetcher, PriceResponse};

#[allow(non_snake_case)]
#[derive(Clone, Debug, Deserialize)]
pub struct BinanceResponse {
	/// The token symbol.
	pub symbol: String,
	/// The current price of the token.
	pub lastPrice: String,
	/// Base currency trade volume in the last 24h (for secondary sources)
	pub volume: String,
}

pub struct BinancePriceFetcher {
	base_url: Url,
	symbols: String,
}

#[async_trait::async_trait]
impl PriceFetcher for BinancePriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> PriceResponse {
		let mut url = self.base_url.join("ticker/24hr").unwrap();
		url.query_pairs_mut().append_pair("symbol", (symbol + "USDT").as_str());

		let res = self._send_request(url).await;

		PriceResponse {
			symbol: res.symbol.clone().replace("USDT", ""),
			price: parse_ether(&res.lastPrice).unwrap(),
			volume: parse_ether(&res.volume).unwrap().into(),
		}
	}

	async fn get_tickers(&self) -> Vec<PriceResponse> {
		let mut url = self.base_url.join("ticker/24hr").unwrap();
		url.query_pairs_mut().append_pair("symbols", self.symbols.as_str());

		reqwest::get(url)
			.await
			.unwrap()
			.json::<Vec<BinanceResponse>>()
			.await
			.unwrap()
			.iter()
			.map(|ticker| PriceResponse {
				symbol: ticker.symbol.clone().replace("USDT", ""),
				price: parse_ether(&ticker.lastPrice).unwrap(),
				volume: parse_ether(&ticker.volume).unwrap().into(),
			})
			.collect()
	}
}

impl BinancePriceFetcher {
	pub async fn new(mut symbols: Vec<String>) -> Self {
		symbols.iter_mut().for_each(|symbol| symbol.push_str("USDT"));

		Self {
			base_url: Url::parse("https://api.binance.com/api/v3/").unwrap(),
			symbols: serde_json::to_string(&symbols).unwrap(),
		}
	}

	async fn _send_request(&self, url: Url) -> BinanceResponse {
		reqwest::get(url).await.unwrap().json::<BinanceResponse>().await.unwrap()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let binance_fetcher = BinancePriceFetcher::new(vec!["BTC".to_string()]).await;
		let res = binance_fetcher.get_ticker_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher =
			BinancePriceFetcher::new(vec!["BTC".to_string(), "ETH".to_string()]).await;
		let res = binance_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
