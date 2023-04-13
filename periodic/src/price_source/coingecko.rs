use cccp_primitives::{
	periodic::{PriceFetcher, PriceResponse},
	sub_display_format,
};
use reqwest::{Response, Url};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::time::{sleep, Duration};

use crate::price_source::LOG_TARGET;

const SUB_LOG_TARGET: &str = "coingecko";

#[derive(Debug, Clone, Deserialize)]
pub struct SupportedCoin {
	pub id: String,
	pub symbol: String,
	pub name: String,
}

pub struct CoingeckoPriceFetcher {
	pub base_url: Url,
	pub ids: Vec<String>,
	pub supported_coins: Vec<SupportedCoin>,
}

#[async_trait::async_trait]
impl PriceFetcher for CoingeckoPriceFetcher {
	async fn get_price_with_symbol(&self, symbol: String) -> String {
		let id = self.get_id_from_symbol(&symbol);
		let url = self
			.base_url
			.join(&format!("simple/price?ids={}&vs_currencies=usd", id))
			.unwrap();

		self._send_request(url)
			.await
			.get(id)
			.expect("Cannot find symbol in response")
			.get("usd")
			.expect("Cannot find usd price in response")
			.to_string()
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		let url = self
			.base_url
			.join(&format!("simple/price?ids={}&vs_currencies=usd", self.ids.join(",")))
			.unwrap();
		let response = self._send_request(url).await;

		self.ids
			.iter()
			.map(|id| {
				let price = response.get(id).unwrap().get("usd").unwrap().to_string();
				let symbol = self
					.supported_coins
					.iter()
					.find(|coin| coin.id == *id)
					.unwrap()
					.symbol
					.to_uppercase();
				PriceResponse { symbol, price }
			})
			.collect()
	}
}

impl CoingeckoPriceFetcher {
	pub async fn new(symbols: Vec<String>) -> Self {
		let support_coin_list: Vec<SupportedCoin> =
			CoingeckoPriceFetcher::get_all_coin_list().await;

		let ids: Vec<String> = symbols
			.iter()
			.filter_map(|symbol| {
				support_coin_list
					.iter()
					.find(|coin| coin.symbol == symbol.to_lowercase())
					.map(|coin| coin.id.clone())
			})
			.collect();

		Self {
			base_url: Url::parse("https://api.coingecko.com/api/v3/").unwrap(),
			ids,
			supported_coins: support_coin_list,
		}
	}

	async fn get_all_coin_list() -> Vec<SupportedCoin> {
		let mut retry_interval = Duration::from_secs(30);
		loop {
			match reqwest::get("https://api.coingecko.com/api/v3/coins/list")
				.await
				.and_then(Response::error_for_status)
			{
				Ok(response) => match response.json::<Vec<SupportedCoin>>().await {
					Ok(mut coins) => {
						coins.retain(|x| &x.name != "Beefy.Finance");
						return coins
					},
					Err(e) => {
						log::error!(
							target: LOG_TARGET,
							"-[{}] ❗️ Error decoding support coin list: {}, Retry in {:?} secs...",
							sub_display_format(SUB_LOG_TARGET),
							e.to_string(),
							retry_interval
						);
						sentry::capture_error(&e);
						sleep(retry_interval).await;
						retry_interval *= 2;
					},
				},
				Err(e) => {
					log::error!(
						target: LOG_TARGET,
						"-[{}] ❗️ Error fetching support coin list: {}, Retry in {:?} secs...",
						sub_display_format(SUB_LOG_TARGET),
						e.to_string(),
						retry_interval
					);
					sentry::capture_error(&e);
					sleep(retry_interval).await;
					retry_interval *= 2;
				},
			}
		}
	}

	async fn _send_request(&self, url: Url) -> HashMap<String, HashMap<String, f64>> {
		let mut retry_interval = Duration::from_secs(30);
		loop {
			match reqwest::get(url.clone()).await.and_then(Response::error_for_status) {
				Ok(response) =>
					match response.json::<HashMap<String, HashMap<String, f64>>>().await {
						Ok(result) => return result,
						Err(e) => {
							log::error!(
								target: LOG_TARGET,
								"-[{}] ❗️ Error decoding coingecko response. Maybe rate limit exceeds?: {}, Retry in {:?} secs...",
								sub_display_format(SUB_LOG_TARGET),
								e.to_string(),
								retry_interval
							);
							sentry::capture_error(&e);
							sleep(retry_interval).await;
							retry_interval *= 2;
						},
					},
				Err(e) => {
					log::error!(
						target: LOG_TARGET,
						"-[{}] ❗️ Error fetching from coingecko: {}, Retry in {:?} secs...",
						sub_display_format(SUB_LOG_TARGET),
						e.to_string(),
						retry_interval,
					);
					sentry::capture_error(&e);
					sleep(retry_interval).await;
					retry_interval *= 2;
				},
			}
		}
	}

	fn get_id_from_symbol(&self, symbol: &str) -> &str {
		self.supported_coins
			.iter()
			.find(|coin| coin.symbol == symbol.to_lowercase())
			.expect("Cannot find symbol in support coin list")
			.id
			.as_str()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let coingecko_fetcher = CoingeckoPriceFetcher::new(vec!["BTC".to_string()]).await;
		let res = coingecko_fetcher.get_price_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher =
			CoingeckoPriceFetcher::new(vec!["BTC".to_string(), "ETH".to_string()]).await;
		let res = binance_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
