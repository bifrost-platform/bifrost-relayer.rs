use std::collections::BTreeMap;

use alloy::primitives::utils::parse_ether;
use eyre::Result;
use reqwest::{Client, Url};
use serde::Deserialize;

use br_primitives::periodic::PriceResponse;

use crate::traits::PriceFetcher;

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

#[derive(Clone)]
pub struct BinancePriceFetcher {
	base_url: Url,
	symbols: String,
	client: Client,
}

#[async_trait::async_trait]
impl PriceFetcher for BinancePriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let mut url = self.base_url.join("ticker/24hr").unwrap();
		url.query_pairs_mut().append_pair("symbol", (symbol + "USDT").as_str());

		let res = self._send_request(url).await?;

		Ok(PriceResponse {
			price: parse_ether(&res.lastPrice)?,
			volume: parse_ether(&res.volume)?.into(),
		})
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let mut url = self.base_url.join("ticker/24hr").unwrap();
		url.query_pairs_mut().append_pair("symbols", self.symbols.as_str());

		let response = self.client.get(url).send().await?.json::<Vec<BinanceResponse>>().await?;

		let mut ret = BTreeMap::new();
		for ticker in &response {
			let price = parse_ether(&ticker.lastPrice)?;
			let volume = parse_ether(&ticker.volume)?.into();

			if ticker.symbol == "BTCUSDT" {
				// BTC ticker from binance is BTCB ticker
				ret.insert("BTCB".into(), PriceResponse { price, volume });
			} else {
				ret.insert(
					ticker.symbol.clone().replace("USDT", ""),
					PriceResponse { price, volume },
				);
			}
		}

		Ok(ret)
	}
}

impl BinancePriceFetcher {
	pub fn new(client: Client) -> Self {
		let mut symbols: Vec<String> = vec!["ETH".into(), "BNB".into(), "POL".into(), "BTC".into()];
		symbols.iter_mut().for_each(|symbol| symbol.push_str("USDT"));

		Self {
			base_url: Url::parse("https://api.binance.com/api/v3/").unwrap(),
			symbols: serde_json::to_string(&symbols).unwrap(),
			client,
		}
	}

	async fn _send_request(&self, url: Url) -> Result<BinanceResponse> {
		Ok(self.client.get(url).send().await?.json::<BinanceResponse>().await?)
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
		let binance_fetcher = BinancePriceFetcher::new(test_client());
		let res = binance_fetcher.get_ticker_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher = BinancePriceFetcher::new(test_client());
		let res = binance_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
