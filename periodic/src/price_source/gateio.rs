use cccp_primitives::periodic::{PriceFetcher, PriceResponse};
use reqwest::Url;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GateioResponse {
	pub currency_pair: String,
	pub last: String,
}

pub struct GateioPriceFetcher {
	base_url: Url,
	symbols: Vec<String>,
}

#[async_trait::async_trait]
impl PriceFetcher for GateioPriceFetcher {
	async fn get_price_with_symbol(&self, symbol: String) -> String {
		let mut url = self.base_url.join("spot/tickers").unwrap();
		url.query_pairs_mut().append_pair("currency_pair", symbol.as_str());

		self._send_request(url).await[0].last.clone()
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		let url = self.base_url.join("spot/tickers").unwrap();
		self._send_request(url)
			.await
			.iter()
			.filter_map(|ticker| {
				if self.symbols.contains(&ticker.currency_pair) {
					Some(PriceResponse {
						symbol: ticker.currency_pair.replace('_', ""),
						price: ticker.last.clone(),
					})
				} else {
					None
				}
			})
			.collect()
	}
}

impl GateioPriceFetcher {
	pub async fn new(symbols: Vec<String>) -> Self {
		Self {
			base_url: Url::parse("https://api.gateio.ws/api/v4/")
				.expect("Failed to parse GateIo URL"),
			symbols,
		}
	}

	async fn _send_request(&self, url: Url) -> Vec<GateioResponse> {
		reqwest::get(url)
			.await
			.expect("Failed to send request to gateio")
			.json::<Vec<GateioResponse>>()
			.await
			.expect("Failed to parse gateio response")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let gateio_fetcher = GateioPriceFetcher::new(vec!["BTC_USDT".to_string()]).await;
		let res = gateio_fetcher.get_price_with_symbol("BTC_USDT".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let gateio_fetcher =
			GateioPriceFetcher::new(vec!["BTC_USDT".to_string(), "ETH_USDT".to_string()]).await;
		let res = gateio_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
