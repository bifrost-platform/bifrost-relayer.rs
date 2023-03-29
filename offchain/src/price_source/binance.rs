use cccp_primitives::offchain::{PriceFetcher, PriceResponse};
use reqwest::Url;

pub struct BinancePriceFetcher {
	base_url: Url,
	symbols: String,
}

#[async_trait::async_trait]
impl PriceFetcher<PriceResponse> for BinancePriceFetcher {
	async fn new(mut symbols: Vec<String>) -> Self {
		for s in symbols.iter_mut() {
			*s = s.replace("_", "");
		}

		Self {
			base_url: Url::parse("https://api.binance.com/api/v3/").unwrap(),
			symbols: serde_json::to_string(&symbols).unwrap(),
		}
	}

	async fn get_price_with_symbol(&self, symbol: String) -> String {
		let mut url = self.base_url.join("ticker/price").unwrap();
		url.query_pairs_mut().append_pair("symbol", symbol.replace("_", "").as_str());

		self._send_request(url).await.price
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		let mut url = self.base_url.join("ticker/price").unwrap();
		url.query_pairs_mut().append_pair("symbols", self.symbols.as_str());

		reqwest::get(url).await.unwrap().json::<Vec<PriceResponse>>().await.unwrap()
	}

	async fn _send_request(&self, url: Url) -> PriceResponse {
		reqwest::get(url).await.unwrap().json::<PriceResponse>().await.unwrap()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetch_price() {
		let binance_fetcher = BinancePriceFetcher::new(vec!["BTC_USDT".to_string()]).await;
		let res = binance_fetcher.get_price_with_symbol("BTC_USDT".to_string()).await;

		println!("{:?}", res);
	}

	#[tokio::test]
	async fn fetch_prices() {
		let binance_fetcher =
			BinancePriceFetcher::new(vec!["BTC_USDT".to_string(), "ETH_USDT".to_string()]).await;
		let res = binance_fetcher.get_price().await;

		println!("{:#?}", res);
	}
}
