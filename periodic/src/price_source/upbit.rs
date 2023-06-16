use ethers::{types::U256, utils::parse_ether};
use reqwest::Url;
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

		self.format_response(self._send_request(url).await[0].clone()).await
	}

	async fn get_tickers(&self) -> Vec<PriceResponse> {
		let mut url = self.base_url.join("ticker").unwrap();
		url.query_pairs_mut().append_pair("markets", self.symbols.as_str());

		let responses = self._send_request(url).await;

		let mut ret = vec![];
		for response in responses {
			ret.push(self.format_response(response).await);
		}

		ret
	}
}

impl UpbitPriceFetcher {
	pub async fn new(symbols: Vec<String>) -> Self {
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

		Self {
			base_url: Url::parse("https://api.upbit.com/v1/").unwrap(),
			symbols: formatted_symbols.join(","),
		}
	}

	async fn format_response(&self, response: UpbitResponse) -> PriceResponse {
		if response.market.contains("KRW-") {
			let usd_price = krw_to_usd(parse_ether(response.trade_price).unwrap()).await;
			PriceResponse {
				symbol: response.market.replace("KRW-", ""),
				price: usd_price,
				volume: parse_ether(response.acc_trade_volume_24h).unwrap().into(),
			}
		} else if response.market.contains("BTC-") {
			let krw_price = self.btc_to_krw(response.trade_price).await;
			let usd_price = krw_to_usd(krw_price).await;

			PriceResponse {
				symbol: response.market.replace("BTC-", ""),
				price: usd_price,
				volume: parse_ether(response.acc_trade_volume_24h).unwrap().into(),
			}
		} else {
			todo!()
		}
	}

	async fn btc_to_krw(&self, btc_amount: f64) -> U256 {
		let btc_price =
			self._send_request(self.base_url.join("ticker?markets=KRW-BTC").unwrap()).await[0]
				.trade_price;

		parse_ether(btc_price * btc_amount).unwrap()
	}

	async fn _send_request(&self, url: Url) -> Vec<UpbitResponse> {
		reqwest::get(url)
			.await
			.expect("Failed to send request to upbit")
			.json::<Vec<UpbitResponse>>()
			.await
			.expect("Failed to parse upbit response")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let upbit_fetcher = UpbitPriceFetcher::new(vec!["BTC".to_string()]).await;
		let res = upbit_fetcher.get_ticker_with_symbol("BFC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let upbit_fetcher =
			UpbitPriceFetcher::new(vec!["BTC".to_string(), "ETH".to_string(), "BFC".to_string()])
				.await;
		let res = upbit_fetcher.get_tickers().await;

		println!("{:#?}", res);
	}

	#[tokio::test]
	async fn btc_krw_conversion() {
		let upbit_fetcher = UpbitPriceFetcher::new(vec![]).await;
		let res = upbit_fetcher.btc_to_krw(0.00000175f64).await;

		println!("{:?}", res);
	}
}
