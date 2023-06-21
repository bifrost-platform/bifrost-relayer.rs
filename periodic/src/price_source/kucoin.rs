use std::collections::BTreeMap;

use ethers::utils::parse_ether;
use reqwest::{Error, Url};
use serde::Deserialize;

use cccp_primitives::{PriceFetcher, PriceResponse};

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
}

#[async_trait::async_trait]
impl PriceFetcher for KucoinPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> PriceResponse {
		let mut url = self.base_url.join("market/stats").unwrap();
		url.query_pairs_mut().append_pair("symbol", (symbol.clone() + "-USDT").as_str());

		let res = &self._send_request(url).await.unwrap().data;

		PriceResponse {
			price: parse_ether(&res.last).unwrap(),
			volume: parse_ether(&res.vol).unwrap().into(),
		}
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		let mut ret = BTreeMap::new();
		for symbol in &self.symbols {
			ret.insert(symbol.clone(), self.get_ticker_with_symbol(symbol.clone()).await);
		}

		Ok(ret)
	}
}

impl KucoinPriceFetcher {
	pub async fn new() -> Result<Self, Error> {
		let symbols: Vec<String> =
			vec!["ETH".into(), "BFC".into(), "BNB".into(), "MATIC".into(), "BIFI".into()];

		Ok(Self {
			base_url: Url::parse("https://api.kucoin.com/api/v1/")
				.expect("Failed to parse KuCoin URL"),
			symbols,
		})
	}

	async fn _send_request(&self, url: Url) -> Result<KucoinResponse, Error> {
		return match reqwest::get(url).await {
			Ok(response) => match response.json::<KucoinResponse>().await {
				Ok(response) => Ok(response),
				Err(e) => Err(e),
			},
			Err(e) => Err(e),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let kucoin_fetcher = KucoinPriceFetcher::new().await;
		let res = kucoin_fetcher.get_ticker_with_symbol("BFC".to_string()).await;

		println!("{:#?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let kucoin_fetcher = KucoinPriceFetcher::new().await;
		let res = kucoin_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
