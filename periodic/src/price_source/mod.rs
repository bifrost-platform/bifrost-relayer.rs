use std::{collections::BTreeMap, sync::Arc};

use alloy::{
	network::Network,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use async_trait::async_trait;
use eyre::Result;

use br_client::eth::EthClient;
use br_primitives::periodic::{PriceResponse, PriceSource};

use crate::{
	price_source::{
		binance::BinancePriceFetcher, chainlink::ChainlinkPriceFetcher,
		coingecko::CoingeckoPriceFetcher, exchange_rate::ExchangeRatePriceFetcher,
		gateio::GateioPriceFetcher, kucoin::KucoinPriceFetcher, upbit::UpbitPriceFetcher,
	},
	traits::PriceFetcher,
};

mod binance;
mod chainlink;
mod coingecko;
mod exchange_rate;
mod gateio;
mod kucoin;
mod upbit;

const LOG_TARGET: &str = "price-fetcher";

#[derive(Clone, PartialEq, Debug)]
pub enum FetchMode {
	/// Standard source for all tokens.
	Standard,
	/// Dedicated source for a specific token. (e.g. "JPYC")
	Dedicated(String),
}

#[derive(Clone)]
pub enum PriceFetchers<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	Binance(BinancePriceFetcher),
	Chainlink(ChainlinkPriceFetcher<F, P, N>),
	CoinGecko(CoingeckoPriceFetcher),
	Gateio(GateioPriceFetcher),
	Kucoin(KucoinPriceFetcher),
	Upbit(UpbitPriceFetcher),
	ExchangeRate(ExchangeRatePriceFetcher),
}

impl<F, P, N: Network> PriceFetchers<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub async fn new(
		exchange: PriceSource,
		client: Option<Arc<EthClient<F, P, N>>>,
		mode: FetchMode,
	) -> Result<Self> {
		match exchange {
			PriceSource::Binance => Ok(PriceFetchers::Binance(BinancePriceFetcher::new().await?)),
			PriceSource::Chainlink => {
				Ok(PriceFetchers::Chainlink(ChainlinkPriceFetcher::new(client, mode).await))
			},
			PriceSource::Coingecko => {
				Ok(PriceFetchers::CoinGecko(CoingeckoPriceFetcher::new().await?))
			},
			PriceSource::Gateio => Ok(PriceFetchers::Gateio(GateioPriceFetcher::new().await?)),
			PriceSource::Kucoin => Ok(PriceFetchers::Kucoin(KucoinPriceFetcher::new().await?)),
			PriceSource::Upbit => Ok(PriceFetchers::Upbit(UpbitPriceFetcher::new().await?)),
			PriceSource::ExchangeRate => {
				Ok(PriceFetchers::ExchangeRate(ExchangeRatePriceFetcher::new(mode)))
			},
		}
	}

	pub fn mode(&self) -> FetchMode {
		match self {
			PriceFetchers::Binance(_) => FetchMode::Standard,
			PriceFetchers::Chainlink(fetcher) => fetcher.mode.clone(),
			PriceFetchers::CoinGecko(_) => FetchMode::Standard,
			PriceFetchers::Gateio(_) => FetchMode::Standard,
			PriceFetchers::Kucoin(_) => FetchMode::Standard,
			PriceFetchers::Upbit(_) => FetchMode::Standard,
			PriceFetchers::ExchangeRate(fetcher) => fetcher.mode.clone(),
		}
	}
}

#[async_trait]
impl<F, P, N: Network> PriceFetcher for PriceFetchers<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		match self {
			PriceFetchers::Binance(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Chainlink(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::CoinGecko(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Gateio(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Kucoin(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::Upbit(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
			PriceFetchers::ExchangeRate(fetcher) => fetcher.get_ticker_with_symbol(symbol).await,
		}
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		match self {
			PriceFetchers::Binance(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Chainlink(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::CoinGecko(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Gateio(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Kucoin(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::Upbit(fetcher) => fetcher.get_tickers().await,
			PriceFetchers::ExchangeRate(fetcher) => fetcher.get_tickers().await,
		}
	}
}
