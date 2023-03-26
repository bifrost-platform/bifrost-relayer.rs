use cccp_primitives::offchain::{PriceFetcher, PriceResponse};

pub struct UpbitPriceFetcher {
	base_url: reqwest::Url,
	symbols: Vec<String>,
}

#[async_trait::async_trait]
impl PriceFetcher for UpbitPriceFetcher {
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
		let upbit_fetcher = UpbitPriceFetcher::new(vec!["BTCUSDT".to_string()]);
		let res = upbit_fetcher.get_price_with_symbol("BTCUSDT".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let upbit_fetcher =
			UpbitPriceFetcher::new(vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
		let res = upbit_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
