use std::{collections::BTreeMap, fmt::Error};

use alloy::primitives::{U256, utils::parse_ether};
use eyre::Result;
use reqwest::Url;
use serde::Deserialize;

use br_primitives::periodic::PriceResponse;

use crate::{price_source::exchange_rate::convert_currency_to_usd, traits::PriceFetcher};

#[derive(Debug, Clone, Deserialize)]
pub struct UpbitResponse {
	/// 종목 구분 코드
	pub market: String,
	/// 종가(현재가)
	pub trade_price: f64,
	/// 24시간 누적 거래량
	pub acc_trade_volume_24h: f64,
}

#[derive(Clone)]
pub struct UpbitPriceFetcher {
	base_url: Url,
	symbols: String,
}

#[async_trait::async_trait]
impl PriceFetcher for UpbitPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let mut url = self.base_url.join("ticker")?;
		if symbol.contains("BFC") {
			url.query_pairs_mut().append_pair("markets", format!("BTC-{}", symbol).as_str());
		} else {
			url.query_pairs_mut().append_pair("markets", format!("KRW-{}", symbol).as_str());
		}

		Ok(self.format_response(self._send_request(url).await?[0].clone()).await?.1)
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let mut url = self.base_url.join("ticker")?;
		url.query_pairs_mut().append_pair("markets", self.symbols.as_str());

		let responses = self._send_request(url).await?;
		let mut ret = BTreeMap::new();
		for response in responses {
			match self.format_response(response).await {
				Ok(formatted_response) => {
					ret.insert(formatted_response.0, formatted_response.1);
				},
				Err(_) => continue,
			}
		}
		Ok(ret)
	}
}

impl UpbitPriceFetcher {
	pub async fn new() -> Result<Self, Error> {
		let symbols: Vec<String> = vec!["ETH".into(), "BFC".into(), "POL".into()];

		let formatted_symbols: Vec<String> = symbols
			.into_iter()
			.map(|symbol| {
				if symbol.contains("BFC") {
					format!("BTC-{}", symbol)
				} else {
					format!("KRW-{}", symbol)
				}
			})
			.collect();

		Ok(Self {
			base_url: Url::parse("https://api.upbit.com/v1/").unwrap(),
			symbols: formatted_symbols.join(","),
		})
	}

	async fn format_response(
		&self,
		response: UpbitResponse,
	) -> Result<(String, PriceResponse), Error> {
		if response.market.contains("KRW-") {
			let amount = parse_ether(&response.trade_price.to_string()).unwrap();
			match convert_currency_to_usd("krw", amount).await {
				Ok(usd_price) => Ok((
					response.market.replace("KRW-", ""),
					PriceResponse {
						price: usd_price,
						volume: parse_ether(&response.acc_trade_volume_24h.to_string())
							.unwrap()
							.into(),
					},
				)),
				Err(_) => Err(Error),
			}
		} else if response.market.contains("BTC-") {
			match self.btc_to_krw(response.trade_price).await {
				Ok(krw_price) => match convert_currency_to_usd("krw", krw_price).await {
					Ok(usd_price) => Ok((
						response.market.replace("BTC-", ""),
						PriceResponse {
							price: usd_price,
							volume: parse_ether(&response.acc_trade_volume_24h.to_string())
								.unwrap()
								.into(),
						},
					)),
					Err(_) => Err(Error),
				},
				Err(_) => Err(Error),
			}
		} else {
			unimplemented!()
		}
	}

	async fn btc_to_krw(&self, btc_amount: f64) -> Result<U256> {
		let response = self._send_request(self.base_url.join("ticker?markets=KRW-BTC")?).await?;
		let btc_price = response[0].trade_price;
		Ok(parse_ether(&(btc_price * btc_amount).to_string())?)
	}

	async fn _send_request(&self, url: Url) -> Result<Vec<UpbitResponse>> {
		Ok(reqwest::get(url).await?.json::<Vec<UpbitResponse>>().await?)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let upbit_fetcher: UpbitPriceFetcher = UpbitPriceFetcher::new().await.unwrap();
		let res = upbit_fetcher.get_ticker_with_symbol("BFC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let upbit_fetcher: UpbitPriceFetcher = UpbitPriceFetcher::new().await.unwrap();
		let res = upbit_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}

	#[tokio::test]
	async fn btc_krw_conversion() {
		let upbit_fetcher: UpbitPriceFetcher = UpbitPriceFetcher::new().await.unwrap();
		let res = upbit_fetcher.btc_to_krw(0.00000175f64).await;

		println!("{:?}", res);
	}
}
