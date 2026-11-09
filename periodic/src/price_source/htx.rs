use std::collections::BTreeMap;

use alloy::primitives::utils::parse_ether;
use eyre::Result;
use reqwest::{Client, Url};
use serde::Deserialize;

use br_primitives::periodic::PriceResponse;

use crate::traits::PriceFetcher;

/// The 24h rolling ticker of a single market, from `market/detail/merged`.
///
/// `market/tickers` is deliberately not used: it returns every market on the exchange and
/// documents `close` as a natural-day (Singapore time) close, whereas the single-market
/// endpoint documents it as the last trade price of the rolling 24h window.
#[derive(Debug, Clone, Deserialize)]
pub struct HtxTick {
	/// The last trade price of the rolling 24h window, in the quote currency.
	pub close: f64,
	/// 24h accumulated trade volume, in the base currency.
	pub amount: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HtxMergedResponse {
	/// "ok" on success, "error" otherwise.
	pub status: String,
	#[serde(rename = "err-msg", default)]
	pub err_msg: String,
	pub tick: Option<HtxTick>,
}

#[derive(Clone)]
pub struct HtxPriceFetcher {
	base_url: Url,
	/// The supported base symbols, upper-cased (e.g. "BFC").
	symbols: Vec<String>,
	client: Client,
}

#[async_trait::async_trait]
impl PriceFetcher for HtxPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let mut url = self.base_url.join("market/detail/merged")?;
		url.query_pairs_mut().append_pair("symbol", &Self::market_symbol(&symbol));

		let response = self.client.get(url).send().await?.json::<HtxMergedResponse>().await?;
		if response.status != "ok" {
			eyre::bail!("HTX returned an error for {}: {}", symbol, response.err_msg);
		}
		let tick = response
			.tick
			.ok_or_else(|| eyre::eyre!("HTX returned no tick for {}", symbol))?;

		Self::format_tick(&tick)
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let mut ret = BTreeMap::new();
		for symbol in &self.symbols {
			ret.insert(symbol.clone(), self.get_ticker_with_symbol(symbol.clone()).await?);
		}
		Ok(ret)
	}
}

impl HtxPriceFetcher {
	pub fn new(client: Client) -> Self {
		Self {
			base_url: Url::parse("https://api.huobi.pro/").expect("Failed to parse HTX URL"),
			symbols: vec!["BFC".into()],
			client,
		}
	}

	/// HTX market symbols are lower-cased base+quote with no separator (e.g. "bfcusdt").
	fn market_symbol(symbol: &str) -> String {
		format!("{}usdt", symbol.to_lowercase())
	}

	fn format_tick(tick: &HtxTick) -> Result<PriceResponse> {
		Ok(PriceResponse {
			price: parse_ether(&tick.close.to_string())?,
			volume: parse_ether(&tick.amount.to_string())?,
		})
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
		let htx_fetcher = HtxPriceFetcher::new(test_client());
		let res = htx_fetcher.get_ticker_with_symbol("BFC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let htx_fetcher = HtxPriceFetcher::new(test_client());
		let res = htx_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}
}
