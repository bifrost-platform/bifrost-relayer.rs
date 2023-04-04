use crate::price_source::{
	binance::BinancePriceFetcher, coingecko::CoingeckoPriceFetcher, gateio::GateioPriceFetcher,
	upbit::UpbitPriceFetcher,
};
use async_trait::async_trait;
use cccp_primitives::periodic::{PriceFetcher, PriceResponse, PriceSource};

pub mod binance;
pub mod chainlink;
pub mod coingecko;
pub mod gateio;
pub mod upbit;

pub enum PriceFetchers {
	Binance(BinancePriceFetcher),
	CoinGecko(CoingeckoPriceFetcher),
	Gateio(GateioPriceFetcher),
	Upbit(UpbitPriceFetcher),
}

impl PriceFetchers {
	pub async fn new(exchange: PriceSource, symbols: Vec<String>) -> Self {
		match exchange {
			PriceSource::Binance => PriceFetchers::Binance(BinancePriceFetcher::new(symbols).await),
			PriceSource::Coingecko =>
				PriceFetchers::CoinGecko(CoingeckoPriceFetcher::new(symbols).await),
			PriceSource::Gateio => PriceFetchers::Gateio(GateioPriceFetcher::new(symbols).await),
			PriceSource::Upbit => PriceFetchers::Upbit(UpbitPriceFetcher::new(symbols).await),
		}
	}
}

#[async_trait]
impl PriceFetcher for PriceFetchers {
	async fn get_price_with_symbol(&self, symbol: String) -> String {
		match self {
			PriceFetchers::Binance(fetcher) => fetcher.get_price_with_symbol(symbol).await,
			PriceFetchers::CoinGecko(fetcher) => fetcher.get_price_with_symbol(symbol).await,
			PriceFetchers::Gateio(fetcher) => fetcher.get_price_with_symbol(symbol).await,
			PriceFetchers::Upbit(fetcher) => fetcher.get_price_with_symbol(symbol).await,
		}
	}

	async fn get_price(&self) -> Vec<PriceResponse> {
		match self {
			PriceFetchers::Binance(fetcher) => fetcher.get_price().await,
			PriceFetchers::CoinGecko(fetcher) => fetcher.get_price().await,
			PriceFetchers::Gateio(fetcher) => fetcher.get_price().await,
			PriceFetchers::Upbit(fetcher) => fetcher.get_price().await,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fetcher_enum_matching() {
		let symbols = vec!["BTC_USDT".to_string(), "ETH_USDT".to_string()];
		let fetchers = vec![
			PriceFetchers::new(PriceSource::Gateio, symbols.clone()).await,
			PriceFetchers::new(PriceSource::Binance, symbols.clone()).await,
		];

		for fetcher in fetchers {
			println!(
				"{:?} {:?}",
				fetcher.get_price_with_symbol("BTC_USDT".to_string()).await,
				fetcher.get_price().await
			);
		}
	}
}
