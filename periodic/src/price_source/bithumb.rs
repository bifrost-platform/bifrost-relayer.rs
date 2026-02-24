use std::collections::BTreeMap;

use alloy::primitives::utils::parse_ether;
use eyre::Result;
use reqwest::{Client, Url};
use serde::Deserialize;

use br_primitives::periodic::PriceResponse;

use crate::{price_source::exchange_rate::convert_currency_to_usd, traits::PriceFetcher};

#[derive(Debug, Clone, Deserialize)]
pub struct BithumbResponse {
	/// The market code (e.g. "KRW-BTC").
	pub market: String,
	/// The current trade price in KRW.
	pub trade_price: f64,
	/// 24h accumulated trade volume.
	pub acc_trade_volume_24h: f64,
}

#[derive(Clone)]
pub struct BithumbPriceFetcher {
	base_url: Url,
	symbols: String,
	client: Client,
}

#[async_trait::async_trait]
impl PriceFetcher for BithumbPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let mut url = self.base_url.join("ticker")?;
		url.query_pairs_mut().append_pair("markets", format!("KRW-{}", symbol).as_str());

		let responses = self._send_request(url).await?;
		Ok(self.format_response(responses[0].clone()).await?.1)
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let mut url = self.base_url.join("ticker")?;
		url.query_pairs_mut().append_pair("markets", self.symbols.as_str());

		let responses = self._send_request(url).await?;
		let mut ret = BTreeMap::new();
		for response in responses {
			if let Ok((symbol, price_response)) = self.format_response(response).await {
				ret.insert(symbol, price_response);
			}
		}
		Ok(ret)
	}
}

impl BithumbPriceFetcher {
	pub fn new(client: Client) -> Self {
		let symbols: Vec<String> = vec!["BFC", "BTC", "ETH", "BNB", "POL"]
			.into_iter()
			.map(|s| format!("KRW-{}", s))
			.collect();

		Self {
			base_url: Url::parse("https://api.bithumb.com/v1/").unwrap(),
			symbols: symbols.join(","),
			client,
		}
	}

	async fn format_response(&self, response: BithumbResponse) -> Result<(String, PriceResponse)> {
		let symbol = response.market.strip_prefix("KRW-").unwrap_or(&response.market).to_string();

		let krw_price = parse_ether(&response.trade_price.to_string())?;
		let usd_price = convert_currency_to_usd(&self.client, "krw", krw_price).await?;
		let volume = parse_ether(&response.acc_trade_volume_24h.to_string())?;

		Ok((symbol, PriceResponse { price: usd_price, volume }))
	}

	async fn _send_request(&self, url: Url) -> Result<Vec<BithumbResponse>> {
		Ok(self.client.get(url).send().await?.json::<Vec<BithumbResponse>>().await?)
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
		let bithumb_fetcher = BithumbPriceFetcher::new(test_client());
		let res = bithumb_fetcher.get_ticker_with_symbol("BFC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let bithumb_fetcher = BithumbPriceFetcher::new(test_client());
		let res = bithumb_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
