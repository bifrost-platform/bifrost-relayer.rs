use std::{collections::BTreeMap, fmt::Error};

use alloy::primitives::utils::parse_ether;
use eyre::Result;
use reqwest::Url;
use serde::Deserialize;

use br_primitives::periodic::PriceResponse;

use crate::traits::PriceFetcher;

#[derive(Debug, Clone, Deserialize)]
pub struct GateioResponse {
	/// Currency pair
	pub currency_pair: String,
	/// Last trading price
	pub last: String,
	/// Base currency trade volume in the last 24h
	pub base_volume: String,
}

#[derive(Clone)]
pub struct GateioPriceFetcher {
	base_url: Url,
	symbols: Vec<String>,
}

#[async_trait::async_trait]
impl PriceFetcher for GateioPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let mut url = self.base_url.join("spot/tickers").unwrap();
		url.query_pairs_mut()
			.append_pair("currency_pair", (symbol.clone() + "_USDT").as_str());

		let res = self._send_request(url).await?[0].clone();

		Ok(PriceResponse {
			price: parse_ether(&res.last).unwrap(),
			volume: parse_ether(&res.base_volume).unwrap().into(),
		})
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let response = self._send_request(self.base_url.join("spot/tickers")?).await?;

		let mut ret = BTreeMap::new();
		response.iter().for_each(|ticker| {
			if self.symbols.contains(&ticker.currency_pair) {
				ret.insert(
					ticker.currency_pair.replace("BIFIF_USDT", "BIFI").replace("_USDT", ""),
					PriceResponse {
						price: parse_ether(&ticker.last).unwrap(),
						volume: parse_ether(&ticker.base_volume).unwrap().into(),
					},
				);
			}
		});

		Ok(ret)
	}
}

impl GateioPriceFetcher {
	pub async fn new() -> Result<Self, Error> {
		let mut symbols: Vec<String> =
			vec!["ETH".into(), "BFC".into(), "BNB".into(), "MATIC".into(), "BIFI".into()];

		symbols.iter_mut().for_each(|symbol| {
			if symbol.contains("BIFI") {
				symbol.push_str("F_USDT");
			} else {
				symbol.push_str("_USDT");
			}
		});

		Ok(Self {
			base_url: Url::parse("https://api.gateio.ws/api/v4/")
				.expect("Failed to parse GateIo URL"),
			symbols,
		})
	}

	async fn _send_request(&self, url: Url) -> Result<Vec<GateioResponse>, Error> {
		match reqwest::get(url).await {
			Ok(response) => match response.json::<Vec<GateioResponse>>().await {
				Ok(ret) => Ok(ret),
				Err(_) => Err(Error),
			},
			Err(_) => Err(Error),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let gateio_fetcher: GateioPriceFetcher = GateioPriceFetcher::new().await.unwrap();
		let res = gateio_fetcher.get_ticker_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let gateio_fetcher: GateioPriceFetcher = GateioPriceFetcher::new().await.unwrap();
		let res = gateio_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
