use cccp_primitives::offchain::{PriceFetcher, PriceResponse};
use reqwest::Url;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct SupportCoin {
	pub id: String,
	pub symbol: String,
	pub name: String,
}

pub struct CoingeckoFetcher {
	pub base_url: Url,
	pub ids: Vec<String>,
	pub support_coin_list: Vec<SupportCoin>,
}

#[async_trait::async_trait]
impl PriceFetcher<HashMap<String, HashMap<String, f64>>> for CoingeckoFetcher {
	async fn new(symbols: Vec<String>) -> Self {
		let support_coin_list = reqwest::get("https://api.coingecko.com/api/v3/coins/list")
			.await
			.expect("Cant get from coingecko api")
			.json::<Vec<SupportCoin>>()
			.await
			.expect("Parsing error on coingecko support coin list");

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

	async fn _send_request(&self, url: Url) -> HashMap<String, HashMap<String, f64>> {
		reqwest::get(url)
			.await
			.expect("Coingecko api server down")
			.json::<HashMap<String, HashMap<String, f64>>>()
			.await
			.expect("Coingecko api rate limit exceeds")
	}
}

impl CoingeckoFetcher {
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
		let coingecko_fetcher = CoingeckoFetcher::new(vec!["BTC".to_string()]).await;
		let res = coingecko_fetcher.get_price_with_symbol("BTC".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher =
			CoingeckoFetcher::new(vec!["BTC".to_string(), "ETH".to_string()]).await;
		let res = binance_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
