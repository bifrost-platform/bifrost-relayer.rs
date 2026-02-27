use std::{collections::BTreeMap, time::Duration};

use alloy::primitives::utils::parse_ether;
use eyre::Result;
use reqwest::{Client, Url};
use serde::Deserialize;
use tokio::time::sleep;

use br_primitives::periodic::PriceResponse;

use crate::traits::PriceFetcher;

#[derive(Clone, Debug, Deserialize)]
pub struct CoinbaseResponse {
	/// Base currency trade volume in the last 24h.
	pub volume: String,
	/// The current price of the token.
	pub price: String,
}

#[derive(Clone)]
pub struct CoinbasePriceFetcher {
	base_url: Url,
	symbols: Vec<String>,
	client: Client,
}

#[async_trait::async_trait]
impl PriceFetcher for CoinbasePriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let url = self.base_url.join(&format!("products/{}-USD/ticker", symbol))?;
		let res = self._send_request(url).await;

		Ok(PriceResponse { price: parse_ether(&res.price)?, volume: parse_ether(&res.volume)? })
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let mut ret = BTreeMap::new();

		for symbol in &self.symbols {
			let url = match self.base_url.join(&format!("products/{}-USD/ticker", symbol)) {
				Ok(url) => url,
				Err(_) => continue,
			};

			let res = self._send_request(url).await;
			if let (Ok(price), Ok(volume)) = (parse_ether(&res.price), parse_ether(&res.volume)) {
				if symbol == "BTC" {
					ret.insert(symbol.clone(), PriceResponse { price, volume });
					ret.insert("CBBTC".into(), PriceResponse { price, volume });
				} else {
					ret.insert(symbol.clone(), PriceResponse { price, volume });
				}
			}
		}

		Ok(ret)
	}
}

impl CoinbasePriceFetcher {
	pub fn new(client: Client) -> Self {
		Self {
			base_url: Url::parse("https://api.exchange.coinbase.com/")
				.expect("Failed to parse Coinbase URL"),
			symbols: vec!["ETH".into(), "BNB".into(), "POL".into(), "BTC".into()],
			client,
		}
	}

	async fn _send_request(&self, url: Url) -> CoinbaseResponse {
		loop {
			match self.client.get(url.clone()).send().await {
				Ok(res) => match res.json::<CoinbaseResponse>().await {
					Ok(res) => return res,
					Err(_) => {},
				},
				Err(_) => {},
			}
			sleep(Duration::from_millis(1000)).await;
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn test_client() -> Client {
		Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
	}

	#[tokio::test]
	async fn fetch_price() {
		let coinbase_fetcher = CoinbasePriceFetcher::new(test_client());
		let res = coinbase_fetcher.get_ticker_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let coinbase_fetcher = CoinbasePriceFetcher::new(test_client());
		let res = coinbase_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
