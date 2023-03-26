use cccp_primitives::offchain::{PriceFetcher, PriceResponse};

pub struct BinancePriceFetcher {
	base_url: reqwest::Url,
	symbols: String,
}

#[async_trait::async_trait]
impl PriceFetcher for BinancePriceFetcher {
	fn new(symbols: Vec<String>) -> Self {
		Self {
			base_url: reqwest::Url::parse("https://api.binance.com/api/v3/").unwrap(),
			symbols: serde_json::to_string(&symbols).unwrap(),
		}
	}

	async fn get_price_with_symbol(&self, symbol: String) -> String {
		let mut url = self.base_url.join("ticker/price").unwrap();
		url.query_pairs_mut().append_pair("symbol", symbol.as_str());

		reqwest::get(url).await.unwrap().json::<PriceResponse>().await.unwrap().price
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		let mut url = self.base_url.join("ticker/price").unwrap();
		url.query_pairs_mut().append_pair("symbols", self.symbols.as_str());

		reqwest::get(url).await.unwrap().json::<Vec<PriceResponse>>().await.unwrap()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let binance_fetcher = BinancePriceFetcher::new(vec!["BTCUSDT".to_string()]);
		let res = binance_fetcher.get_price_with_symbol("BTCUSDT".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher =
			BinancePriceFetcher::new(vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
		let res = binance_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
