use std::{collections::BTreeMap, fmt::Error, marker::PhantomData};

use ethers::{providers::JsonRpcClient, utils::parse_ether};
use reqwest::Url;
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
pub struct BinancePriceFetcher<T> {
	base_url: Url,
	symbols: String,
	_phantom: PhantomData<T>,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PriceFetcher for BinancePriceFetcher<T> {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse, Error> {
		let mut url = self.base_url.join("ticker/24hr").unwrap();
		url.query_pairs_mut().append_pair("symbol", (symbol + "USDT").as_str());

		let res = self._send_request(url).await;

		Ok(PriceResponse {
			price: parse_ether(&res.lastPrice).unwrap(),
			volume: parse_ether(&res.volume).unwrap().into(),
		})
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		let mut url = self.base_url.join("ticker/24hr").unwrap();
		url.query_pairs_mut().append_pair("symbols", self.symbols.as_str());

		let mut ret = BTreeMap::new();
		match reqwest::get(url).await {
			Ok(response) => match response.json::<Vec<BinanceResponse>>().await {
				Ok(binance_response) => binance_response.iter().for_each(|ticker| {
					if ticker.symbol == "BTC" {
						// BTC ticker from binance is BTCB ticker
						ret.insert(
							"BTCB".into(),
							PriceResponse {
								price: parse_ether(&ticker.lastPrice).unwrap(),
								volume: parse_ether(&ticker.volume).unwrap().into(),
							},
						);
					} else {
						ret.insert(
							ticker.symbol.clone().replace("USDT", ""),
							PriceResponse {
								price: parse_ether(&ticker.lastPrice).unwrap(),
								volume: parse_ether(&ticker.volume).unwrap().into(),
							},
						);
					}
				}),
				Err(_) => return Err(Error),
			},
			Err(_) => return Err(Error),
		};

		Ok(ret)
	}
}

impl<T: JsonRpcClient> BinancePriceFetcher<T> {
	pub async fn new() -> Result<BinancePriceFetcher<T>, Error> {
		let mut symbols: Vec<String> =
			vec!["ETH".into(), "BNB".into(), "MATIC".into(), "BTC".into()];
		symbols.iter_mut().for_each(|symbol| symbol.push_str("USDT"));

		Ok(Self {
			base_url: Url::parse("https://api.binance.com/api/v3/").unwrap(),
			symbols: serde_json::to_string(&symbols).unwrap(),
			_phantom: PhantomData,
		})
	}

	async fn _send_request(&self, url: Url) -> BinanceResponse {
		reqwest::get(url).await.unwrap().json::<BinanceResponse>().await.unwrap()
	}
}

#[cfg(test)]
mod tests {
	use ethers::providers::Http;

	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let binance_fetcher: BinancePriceFetcher<Http> = BinancePriceFetcher::new().await.unwrap();
		let res = binance_fetcher.get_ticker_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher: BinancePriceFetcher<Http> = BinancePriceFetcher::new().await.unwrap();
		let res = binance_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
