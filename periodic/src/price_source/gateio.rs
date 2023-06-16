use ethers::utils::parse_ether;
use reqwest::Url;
use serde::Deserialize;

use cccp_primitives::periodic::{PriceFetcher, PriceResponse};

#[derive(Debug, Clone, Deserialize)]
pub struct GateioResponse {
	/// Currency pair
	pub currency_pair: String,
	/// Last trading price
	pub last: String,
	/// Base currency trade volume in the last 24h
	pub base_volume: String,
}

pub struct GateioPriceFetcher {
	base_url: Url,
	symbols: Vec<String>,
}

#[async_trait::async_trait]
impl PriceFetcher for GateioPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> PriceResponse {
		let mut url = self.base_url.join("spot/tickers").unwrap();
		url.query_pairs_mut()
			.append_pair("currency_pair", (symbol.clone() + "_USDT").as_str());

		let res = &self._send_request(url).await[0];

		PriceResponse {
			symbol,
			price: parse_ether(&res.last).unwrap(),
			volume: parse_ether(&res.base_volume).unwrap().into(),
		}
	}

	async fn get_tickers(&self) -> Vec<PriceResponse> {
		let url = self.base_url.join("spot/tickers").unwrap();
		self._send_request(url)
			.await
			.iter()
			.filter_map(|ticker| {
				if self.symbols.contains(&ticker.currency_pair) {
					Some(PriceResponse {
						symbol: ticker.currency_pair.replace("_USDT", ""),
						price: parse_ether(&ticker.last).unwrap(),
						volume: parse_ether(&ticker.base_volume).unwrap().into(),
					})
				} else {
					None
				}
			})
			.collect()
	}
}

impl GateioPriceFetcher {
	pub async fn new(mut symbols: Vec<String>) -> Self {
		symbols.iter_mut().for_each(|symbol| {
			if symbol.contains("BIFI") {
				symbol.push_str("F_USDT");
			} else {
				symbol.push_str("_USDT");
			}
		});

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
		let gateio_fetcher = GateioPriceFetcher::new(vec!["BTC".to_string()]).await;
		let res = gateio_fetcher.get_ticker_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let gateio_fetcher =
			GateioPriceFetcher::new(vec!["BTC".to_string(), "BIFI".to_string(), "BFC".to_string()])
				.await;
		let res = gateio_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
