use cccp_primitives::offchain::{PriceFetcher, PriceResponse};
use reqwest::Url;
use serde::Deserialize;
use std::collections::HashMap;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Deserialize)]
pub struct SupportCoin {
	pub id: String,
	pub symbol: String,
	pub name: String,
}

pub struct CoingeckoPriceFetcher {
	pub base_url: Url,
	pub ids: Vec<String>,
	pub support_coin_list: Vec<SupportCoin>,
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
			.filter_map(|id| {
				let price = response.get(id).unwrap().get("usd").unwrap().to_string();
				let symbol = self
					.support_coin_list
					.iter()
					.find(|coin| coin.id == *id)
					.unwrap()
					.symbol
					.to_uppercase();
				Some(PriceResponse { symbol, price })
			})
			.collect()
	}
}

impl CoingeckoPriceFetcher {
	pub async fn new(symbols: Vec<String>) -> Self {
		let support_coin_list: Vec<SupportCoin> = CoingeckoPriceFetcher::get_all_coin_list().await;

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
			support_coin_list,
		}
	}

	async fn get_all_coin_list() -> Vec<SupportCoin> {
		let mut retry_interval = Duration::from_secs(30);
		loop {
			match reqwest::get("https://api.coingecko.com/api/v3/coins/list").await {
				Ok(response) => match response.json::<Vec<SupportCoin>>().await {
					Ok(mut coins) => {
						coins.retain(|x| &x.name != "Beefy.Finance");
						return coins
					},
					Err(e) => {
						println!("Error decoding support coin list: {}", e);
						println!("Retry in {:?} secs...", retry_interval);
						sleep(retry_interval).await;
						retry_interval *= 2;
					},
				},
				Err(e) => {
					println!("Error fetching support coin list: {}", e);
					println!("Retry in {:?} secs...", retry_interval);
					sleep(retry_interval).await;
					retry_interval *= 2;
				},
			}
		}
	}

	async fn _send_request(&self, url: Url) -> HashMap<String, HashMap<String, f64>> {
		let mut retry_interval = Duration::from_secs(30);
		loop {
			match reqwest::get(url.clone()).await {
				Ok(response) =>
					match response.json::<HashMap<String, HashMap<String, f64>>>().await {
						Ok(result) => return result,
						Err(e) => {
							println!(
								"Error decoding coingecko response. Maybe rete limit exceeds?: {}",
								e
							);
							println!("Retry in {:?} secs...", retry_interval);
							sleep(retry_interval).await;
							retry_interval *= 2;
						},
					},
				Err(e) => {
					println!("Error fetching from coingecko: {}", e);
					println!("Retry in {:?} secs...", retry_interval);
					sleep(retry_interval).await;
					retry_interval *= 2;
				},
			}
		}
	}

	fn get_id_from_symbol(&self, symbol: &str) -> &str {
		self.support_coin_list
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
