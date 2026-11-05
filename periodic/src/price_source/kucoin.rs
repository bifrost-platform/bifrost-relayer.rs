use std::{collections::BTreeMap, fmt::Error};

use alloy::primitives::utils::parse_ether;
use br_primitives::periodic::PriceResponse;
use eyre::Result;
use reqwest::{Client, Url};
use serde::Deserialize;

use crate::traits::PriceFetcher;

#[derive(Debug, Clone, Deserialize)]
struct Inner {
	/// Last traded price
	last: String,
	/// 24h volume, executed based on base currency
	vol: String,
}

#[derive(Debug, Clone, Deserialize)]
struct KucoinResponse {
	pub data: Inner,
}

#[derive(Clone)]
pub struct KucoinPriceFetcher {
	base_url: Url,
	symbols: Vec<String>,
	client: Client,
}

#[async_trait::async_trait]
impl PriceFetcher for KucoinPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let mut url = self.base_url.join("market/stats")?;
		url.query_pairs_mut().append_pair("symbol", (symbol.clone() + "-USDT").as_str());

		let res = &self._send_request(url).await?.data;

		Ok(PriceResponse { price: parse_ether(&res.last)?, volume: parse_ether(&res.vol)? })
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let mut ret = BTreeMap::new();
		for symbol in &self.symbols {
			ret.insert(symbol.clone(), self.get_ticker_with_symbol(symbol.clone()).await?);
		}

		Ok(ret)
	}
}

impl KucoinPriceFetcher {
	pub fn new(client: Client) -> Self {
		// BFC-USDT is no longer listed on KuCoin (the stats endpoint returns all-null fields),
		// and one failing symbol fails the whole `get_tickers` call.
		let symbols: Vec<String> = vec!["ETH".into(), "BNB".into(), "POL".into()];

		Self {
			base_url: Url::parse("https://api.kucoin.com/api/v1/")
				.expect("Failed to parse KuCoin URL"),
			symbols,
			client,
		}
	}

	async fn _send_request(&self, url: Url) -> Result<KucoinResponse, Error> {
		match self.client.get(url).send().await {
			Ok(response) => match response.json::<KucoinResponse>().await {
				Ok(response) => Ok(response),
				Err(_) => Err(Error),
			},
			Err(_) => Err(Error),
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
		let kucoin_fetcher = KucoinPriceFetcher::new(test_client());
		let res = kucoin_fetcher.get_ticker_with_symbol("ETH".to_string()).await;

		println!("{:#?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let kucoin_fetcher = KucoinPriceFetcher::new(test_client());
		let res = kucoin_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
