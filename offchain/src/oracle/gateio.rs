use cccp_primitives::offchain::{PriceFetcher, PriceResponse};
use serde_json::Value;

pub struct GateioPriceFetcher {
	base_url: reqwest::Url,
	symbols: Vec<String>,
}

#[async_trait::async_trait]
impl PriceFetcher for GateioPriceFetcher {
	fn new(symbols: Vec<String>) -> Self {
		todo!()
	}

	async fn get_price_with_symbol(&self, symbol: String) -> String {
		todo!()
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		todo!()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let gateio_fetcher = GateioPriceFetcher::new(vec!["BTCUSDT".to_string()]);
		let res = gateio_fetcher.get_price_with_symbol("BTCUSDT".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let gateio_fetcher =
			GateioPriceFetcher::new(vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
		let res = gateio_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
