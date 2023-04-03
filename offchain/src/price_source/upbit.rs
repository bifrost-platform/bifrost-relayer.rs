use cccp_primitives::offchain::{PriceFetcher, PriceResponse};
use reqwest::Url;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct UpbitResponse {
	pub market: String,
	pub trade_price: f64,
}

pub struct UpbitPriceFetcher {
	base_url: Url,
	symbols: String,
}

#[async_trait::async_trait]
impl PriceFetcher for UpbitPriceFetcher {
	async fn get_price_with_symbol(&self, symbol: String) -> String {
		let mut url = self.base_url.join("ticker").unwrap();
		url.query_pairs_mut().append_pair("markets", to_upbit_symbol(&symbol).as_str());

		self._send_request(url).await[0].trade_price.to_string()
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		let mut url = self.base_url.join("ticker").unwrap();
		url.query_pairs_mut().append_pair("markets", self.symbols.as_str());

		self._send_request(url)
			.await
			.iter()
			.map(|res| PriceResponse {
				symbol: to_common_symbol(&res.market),
				price: res.trade_price.to_string(),
			})
			.collect()
	}
}

impl UpbitPriceFetcher {
	pub async fn new(symbols: Vec<String>) -> Self {
		let symbols_flipped: Vec<String> =
			symbols.into_iter().map(|symbol| to_upbit_symbol(&symbol)).collect();

		Self {
			base_url: Url::parse("https://api.upbit.com/v1/").unwrap(),
			symbols: symbols_flipped.join(",").to_string(),
		}
	}

	async fn _send_request(&self, url: Url) -> Vec<UpbitResponse> {
		reqwest::get(url)
			.await
			.expect("Failed to send request to upbit")
			.json::<Vec<UpbitResponse>>()
			.await
			.expect("Failed to parse upbit response")
	}
}

fn to_upbit_symbol(symbol: &str) -> String {
	let parts: Vec<&str> = symbol.split('_').collect();
	format!("{}-{}", parts[1], parts[0])
}

fn to_common_symbol(symbol: &str) -> String {
	let parts: Vec<&str> = symbol.split('-').collect();
	format!("{}_{}", parts[1], parts[0])
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let upbit_fetcher = UpbitPriceFetcher::new(vec!["BTC_USDT".to_string()]).await;
		let res = upbit_fetcher.get_price_with_symbol("BTC_USDT".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let upbit_fetcher =
			UpbitPriceFetcher::new(vec!["BTC_USDT".to_string(), "ETH_USDT".to_string()]).await;
		let res = upbit_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
