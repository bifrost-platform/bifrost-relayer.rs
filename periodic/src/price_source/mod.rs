use std::{collections::BTreeMap, fmt::Error, ops::Mul, sync::Arc};

use async_trait::async_trait;
use ethers::{providers::JsonRpcClient, types::U256};
use serde::Deserialize;

use cccp_client::eth::EthClient;
use cccp_primitives::periodic::{PriceFetcher, PriceResponse, PriceSource};

use crate::price_source::{
	binance::BinancePriceFetcher, chainlink::ChainlinkPriceFetcher,
	coingecko::CoingeckoPriceFetcher, gateio::GateioPriceFetcher, kucoin::KucoinPriceFetcher,
	upbit::UpbitPriceFetcher,
};

pub mod binance;
pub mod chainlink;
pub mod coingecko;
pub mod gateio;
pub mod kucoin;
pub mod upbit;

pub const LOG_TARGET: &str = "price-fetcher";

#[derive(Clone)]
pub enum PriceFetchers<T> {
	Binance(BinancePriceFetcher<T>),
	Chainlink(ChainlinkPriceFetcher<T>),
	CoinGecko(CoingeckoPriceFetcher<T>),
	Gateio(GateioPriceFetcher<T>),
	Kucoin(KucoinPriceFetcher<T>),
	Upbit(UpbitPriceFetcher<T>),
}

#[derive(Deserialize)]
struct CurrencyResponse {
	usd: f64,
}

/// Outputs the `usd * 10**18 price` of the `krw * 10**18 price` entered.
pub async fn krw_to_usd(krw_amount: U256) -> Result<U256, Error> {
	return match reqwest::get(
		"https://cdn.jsdelivr.net/gh/fawazahmed0/currency-api@1/latest/currencies/krw/usd.min.json",
	)
	.await
	{
		Ok(response) => match response.json::<CurrencyResponse>().await {
			Ok(exchange_rate_float) => {
				let exchange_rate_decimal: u32 = {
					let rate_str = exchange_rate_float.usd.to_string();
					if let Some(decimal_index) = rate_str.find('.') {
						(rate_str.len() - decimal_index - 1) as u32
					} else {
						0
					}
				};

				let exchange_rate = U256::from(
					exchange_rate_float.usd.mul((10u64.pow(exchange_rate_decimal)) as f64) as u64,
				);

				Ok(krw_amount
					.mul(exchange_rate)
					.checked_div(U256::from(10u64.pow(exchange_rate_decimal)))
					.unwrap())
			},
			Err(_) => Err(Error::default()),
		},
		Err(_) => Err(Error::default()),
	}
}

impl<T: JsonRpcClient> PriceFetchers<T> {
	pub async fn new(
		exchange: PriceSource,
		client: Option<Arc<EthClient<T>>>,
	) -> Result<Self, Error> {
		match exchange {
			PriceSource::Binance => Ok(PriceFetchers::Binance(BinancePriceFetcher::new().await?)),
			PriceSource::Chainlink =>
				Ok(PriceFetchers::Chainlink(ChainlinkPriceFetcher::new(client).await.into())),
			PriceSource::Coingecko =>
				Ok(PriceFetchers::CoinGecko(CoingeckoPriceFetcher::new().await?)),
			PriceSource::Gateio => Ok(PriceFetchers::Gateio(GateioPriceFetcher::new().await?)),
			PriceSource::Kucoin => Ok(PriceFetchers::Kucoin(KucoinPriceFetcher::new().await?)),
			PriceSource::Upbit => Ok(PriceFetchers::Upbit(UpbitPriceFetcher::new().await?)),
		}
	}
}

#[async_trait]
impl<T: JsonRpcClient + 'static> PriceFetcher for PriceFetchers<T> {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse, Error> {
		match self {
			PriceFetchers::Binance(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Chainlink(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::CoinGecko(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Gateio(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Kucoin(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Upbit(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
		}
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		match self {
			PriceFetchers::Binance(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Chainlink(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::CoinGecko(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Gateio(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Kucoin(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Upbit(fetcher) => fetcher.get_tickers().await,
		}
	}
}

#[cfg(test)]
mod tests {
	use ethers::{providers::Http, utils::parse_ether};

	use super::*;

	#[tokio::test]
	async fn fetcher_enum_matching() {
		let fetchers: Vec<PriceFetchers<Http>> = vec![
			PriceFetchers::new(PriceSource::Binance, None).await.unwrap(),
			PriceFetchers::new(PriceSource::Coingecko, None).await.unwrap(),
			PriceFetchers::new(PriceSource::Gateio, None).await.unwrap(),
			PriceFetchers::new(PriceSource::Kucoin, None).await.unwrap(),
			PriceFetchers::new(PriceSource::Upbit, None).await.unwrap(),
		];

		for fetcher in fetchers {
			println!("{:?}", fetcher.get_tickers().await);
		}
	}

	#[tokio::test]
	async fn krw_to_usd_exchange() {
		let res = krw_to_usd(parse_ether(1).unwrap()).await;
		println!("{:?}", res);
	}
}
