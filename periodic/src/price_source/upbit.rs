use std::collections::BTreeMap;

use ethers::{types::U256, utils::parse_ether};
use reqwest::{Error, Url};
use serde::Deserialize;

use cccp_primitives::periodic::{PriceFetcher, PriceResponse};

use crate::price_source::krw_to_usd;

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
	async fn get_ticker_with_symbol(&self, symbol: String) -> PriceResponse {
		let mut url = self.base_url.join("ticker").unwrap();
		if symbol.contains("BFC") {
			url.query_pairs_mut().append_pair("markets", format!("BTC-{}", symbol).as_str());
		} else {
			url.query_pairs_mut().append_pair("markets", format!("KRW-{}", symbol).as_str());
		}

		self.format_response(self._send_request(url).await.unwrap()[0].clone())
			.await
			.unwrap()
			.1
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		let mut url = self.base_url.join("ticker").unwrap();
		url.query_pairs_mut().append_pair("markets", self.symbols.as_str());

		return match self._send_request(url).await {
			Ok(responses) => {
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
			},
			Err(e) => Err(e),
		}
	}
}

impl UpbitPriceFetcher {
	pub async fn new() -> Result<Self, Error> {
		let symbols: Vec<String> = vec!["ETH".into(), "BFC".into(), "MATIC".into()];

		let formatted_symbols: Vec<String> = symbols
			.into_iter()
			.map(|symbol| {
				if symbol.contains("BFC") {
					format!("BTC-{}", symbol).to_string()
				} else {
					format!("KRW-{}", symbol).to_string()
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
			return match krw_to_usd(parse_ether(response.trade_price).unwrap()).await {
				Ok(usd_price) => Ok((
					response.market.replace("KRW-", ""),
					PriceResponse {
						price: usd_price,
						volume: parse_ether(response.acc_trade_volume_24h).unwrap().into(),
					},
				)),
				Err(e) => Err(e),
			}
		} else if response.market.contains("BTC-") {
			return match self.btc_to_krw(response.trade_price).await {
				Ok(krw_price) => match krw_to_usd(krw_price).await {
					Ok(usd_price) => Ok((
						response.market.replace("BTC-", ""),
						PriceResponse {
							price: usd_price,
							volume: parse_ether(response.acc_trade_volume_24h).unwrap().into(),
						},
					)),
					Err(e) => Err(e),
				},
				Err(e) => Err(e),
			}
		} else {
			todo!()
		}
	}

	async fn btc_to_krw(&self, btc_amount: f64) -> Result<U256, Error> {
		return match self._send_request(self.base_url.join("ticker?markets=KRW-BTC").unwrap()).await
		{
			Ok(response) => {
				let btc_price = response[0].trade_price;
				Ok(parse_ether(btc_price * btc_amount).unwrap())
			},
			Err(e) => Err(e),
		}
	}

	async fn _send_request(&self, url: Url) -> Result<Vec<UpbitResponse>, Error> {
		return match reqwest::get(url).await {
			Ok(response) => match response.json::<Vec<UpbitResponse>>().await {
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
		let upbit_fetcher = UpbitPriceFetcher::new().await.unwrap();
		let res = upbit_fetcher.get_ticker_with_symbol("BFC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let upbit_fetcher = UpbitPriceFetcher::new().await.unwrap();
		let res = upbit_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}

	#[tokio::test]
	async fn btc_krw_conversion() {
		let upbit_fetcher = UpbitPriceFetcher::new().await.unwrap();
		let res = upbit_fetcher.btc_to_krw(0.00000175f64).await;

		println!("{:?}", res);
	}
}
