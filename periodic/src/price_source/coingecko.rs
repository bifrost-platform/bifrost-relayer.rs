use std::{collections::BTreeMap, fmt::Error};

use alloy::primitives::utils::parse_ether;
use eyre::Result;
use reqwest::{Client, Response, Url};
use serde::Deserialize;
use tokio::time::{Duration, sleep};

use br_primitives::{periodic::PriceResponse, utils::sub_display_format};

use crate::{price_source::LOG_TARGET, traits::PriceFetcher};

const SUB_LOG_TARGET: &str = "coingecko";

#[derive(Debug, Clone, Deserialize)]
pub struct SupportedCoin {
	pub id: String,
	pub symbol: String,
}

#[derive(Clone)]
pub struct CoingeckoPriceFetcher {
	pub base_url: Url,
	pub ids: Vec<String>,
	pub supported_coins: Vec<SupportedCoin>,
	client: Client,
}

#[async_trait::async_trait]
impl PriceFetcher for CoingeckoPriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		let id = self.get_id_from_symbol(&symbol);
		let url = self
			.base_url
			.join(&format!("simple/price?ids={}&vs_currencies=usd", id))
			.unwrap();

		let price = *self
			._send_request(url)
			.await?
			.get(id)
			.expect("Cannot find symbol in response")
			.get("usd")
			.expect("Cannot find usd price in response");

		Ok(PriceResponse { price: parse_ether(&price.to_string())?, volume: None })
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let url = self
			.base_url
			.join(&format!("simple/price?ids={}&vs_currencies=usd", self.ids.join(",")))
			.unwrap();

		let response = self._send_request(url).await?;

		let mut ret = BTreeMap::new();
		self.ids.iter().for_each(|id| {
			let price = response.get(id).unwrap().get("usd").unwrap();
			let symbol = self
				.supported_coins
				.iter()
				.find(|coin| coin.id == *id)
				.unwrap()
				.symbol
				.to_uppercase();
			ret.insert(
				symbol,
				PriceResponse { price: parse_ether(&price.to_string()).unwrap(), volume: None },
			);
		});

		Ok(ret)
	}
}

impl CoingeckoPriceFetcher {
	pub async fn new() -> Result<Self, Error> {
		let ids: Vec<String> = vec![
			"ethereum".into(),
			"bifrost".into(),
			"binancecoin".into(),
			"polygon-ecosystem-token".into(),
			"usd-coin".into(),
			"bifi".into(),
			"tether".into(),
			"dai".into(),
			"bitcoin".into(),
			"wrapped-bitcoin".into(),
			"binance-bitcoin".into(),
			"coinbase-wrapped-btc".into(),
			"jpy-coin".into(),
		];

		let client = Client::builder()
			.user_agent(format!("bifrost-relayer/br-periodic/{}", env!("CARGO_PKG_VERSION")))
			.build()
			.unwrap();

		let support_coin_list: Vec<SupportedCoin> = Self::get_all_coin_list(&client)
			.await?
			.into_iter()
			.filter(|coin| ids.contains(&coin.id))
			.collect();

		Ok(Self {
			base_url: Url::parse("https://api.coingecko.com/api/v3/").unwrap(),
			ids,
			supported_coins: support_coin_list,
			client,
		})
	}

	async fn get_all_coin_list(client: &Client) -> Result<Vec<SupportedCoin>, Error> {
		let retry_interval = Duration::from_secs(60);
		let mut retries_remaining = 2u8;

		loop {
			match client
				.get("https://api.coingecko.com/api/v3/coins/list")
				.send()
				.await
				.and_then(Response::error_for_status)
			{
				Ok(response) => {
					return match response.json::<Vec<SupportedCoin>>().await {
						Ok(coins) => Ok(coins),
						Err(e) => {
							log::error!(
								target: LOG_TARGET,
								"-[{}] ❗️ Error decoding support coin list: {}, Retry in {:?} secs...",
								sub_display_format(SUB_LOG_TARGET),
								e.to_string(),
								retry_interval
							);
							Err(Error)
						},
					};
				},
				Err(e) => {
					if retries_remaining == 0 {
						log::error!(
							target: LOG_TARGET,
							"-[{}] ❗️ Failed to fetch support coin list after all retries: {}",
							sub_display_format(SUB_LOG_TARGET),
							e.to_string(),
						);
						sentry::capture_error(&e);
						return Err(Error);
					}

					log::warn!(
						target: LOG_TARGET,
						"-[{}] ❗️ Error fetching support coin list: {}, Retry in {:?} secs... Retries left: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						e.to_string(),
						retry_interval,
						retries_remaining,
					);
					sentry::capture_error(&e);
					sleep(retry_interval).await;
					retries_remaining -= 1;
				},
			}
		}
	}

	async fn _send_request(
		&self,
		url: Url,
	) -> Result<BTreeMap<String, BTreeMap<String, f64>>, Error> {
		let retry_interval = Duration::from_secs(60);
		let mut retries_remaining = 2u8;

		loop {
			match self.client.get(url.clone()).send().await.and_then(Response::error_for_status) {
				Ok(response) => {
					return match response.json::<BTreeMap<String, BTreeMap<String, f64>>>().await {
						Ok(result) => Ok(result),
						Err(e) => {
							log::error!(
								target: LOG_TARGET,
								"-[{}] ❗️ Error decoding coingecko response: {}, retry in secondary sources",
								sub_display_format(SUB_LOG_TARGET),
								e.to_string(),
							);
							Err(Error)
						},
					};
				},
				Err(e) => {
					if retries_remaining == 0 {
						return Err(Error);
					}

					log::warn!(
						target: LOG_TARGET,
						"-[{}] ❗️ Error fetching from coingecko: {}, Retry in {:?} secs... Retries left: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						e.to_string(),
						retry_interval,
						retries_remaining,
					);
					sentry::capture_error(&e);
					sleep(retry_interval).await;
					retries_remaining -= 1;
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
	async fn test_coingecko_request_methods() {
		println!("\n=== Testing CoinGecko API Request Methods ===\n");

		// Test 1: Current implementation - reqwest::get()
		println!("Test 1: reqwest::get() [CURRENT CODE]");
		match reqwest::get("https://api.coingecko.com/api/v3/coins/list")
			.await
			.and_then(Response::error_for_status)
		{
			Ok(resp) => {
				println!("✅ Status: {}", resp.status());
				let coins: Result<Vec<SupportedCoin>, _> = resp.json().await;
				match coins {
					Ok(c) => println!("   Got {} coins", c.len()),
					Err(e) => println!("   JSON parse error: {}", e),
				}
			},
			Err(e) => {
				println!("❌ FAILED: {}", e);
				if let Some(status) = e.status() {
					println!("   HTTP Status: {}", status);
				}
			},
		}

		println!();

		// Test 2: Client with explicit User-Agent
		println!("Test 2: Client::new() with custom User-Agent");
		let user_agent = format!("bifrost-relayer/br-periodic/{}", env!("CARGO_PKG_VERSION"));
		println!("User-Agent: {}", user_agent);
		let client = reqwest::Client::builder().user_agent(user_agent).build().unwrap();

		match client
			.get("https://api.coingecko.com/api/v3/coins/list")
			.send()
			.await
			.and_then(Response::error_for_status)
		{
			Ok(resp) => {
				println!("✅ Status: {}", resp.status());
				let coins: Result<Vec<SupportedCoin>, _> = resp.json().await;
				match coins {
					Ok(c) => println!("   Got {} coins", c.len()),
					Err(e) => println!("   JSON parse error: {}", e),
				}
			},
			Err(e) => {
				println!("❌ FAILED: {}", e);
				if let Some(status) = e.status() {
					println!("   HTTP Status: {}", status);
				}
			},
		}

		println!();

		// Test 3: Client with Accept header
		println!("Test 3: Client with Accept header");
		let client = reqwest::Client::new();
		match client
			.get("https://api.coingecko.com/api/v3/coins/list")
			.header("Accept", "*/*")
			.send()
			.await
			.and_then(Response::error_for_status)
		{
			Ok(resp) => {
				println!("✅ Status: {}", resp.status());
				let coins: Result<Vec<SupportedCoin>, _> = resp.json().await;
				match coins {
					Ok(c) => println!("   Got {} coins", c.len()),
					Err(e) => println!("   JSON parse error: {}", e),
				}
			},
			Err(e) => {
				println!("❌ FAILED: {}", e);
				if let Some(status) = e.status() {
					println!("   HTTP Status: {}", status);
				}
			},
		}

		println!();

		// Test 4: Default Client::new() without any headers
		println!("Test 4: Client::new() plain (no custom headers)");
		let client = reqwest::Client::new();
		match client
			.get("https://api.coingecko.com/api/v3/coins/list")
			.send()
			.await
			.and_then(Response::error_for_status)
		{
			Ok(resp) => {
				println!("✅ Status: {}", resp.status());
				let coins: Result<Vec<SupportedCoin>, _> = resp.json().await;
				match coins {
					Ok(c) => println!("   Got {} coins", c.len()),
					Err(e) => println!("   JSON parse error: {}", e),
				}
			},
			Err(e) => {
				println!("❌ FAILED: {}", e);
				if let Some(status) = e.status() {
					println!("   HTTP Status: {}", status);
				}
			},
		}

		println!("\n=== Test Complete ===\n");
	}
}
