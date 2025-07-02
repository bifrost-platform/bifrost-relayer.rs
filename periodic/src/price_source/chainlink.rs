use std::{collections::BTreeMap, sync::Arc};

use crate::traits::PriceFetcher;
use alloy::{
	network::Network,
	primitives::U256,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use br_client::eth::EthClient;
use br_primitives::periodic::PriceResponse;
use eyre::{Result, eyre};

#[derive(Clone)]
pub struct ChainlinkPriceFetcher<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	client: Option<Arc<EthClient<F, P, N>>>,
}

#[async_trait::async_trait]
impl<F, P, N: Network> PriceFetcher for ChainlinkPriceFetcher<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Get the price of a symbol from Chainlink aggregator.
	/// Available symbols: USDC, USDT, DAI, BTC, WBTC, CBBTC
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		match &self.client {
			Some(client) => {
				let symbol_str = symbol.as_str();

				match symbol_str {
					"USDC" | "USDT" | "DAI" | "BTC" | "WBTC" | "CBBTC" => {
						if let Some(contract) = match symbol_str {
							"USDC" => &client.aggregator_contracts.chainlink_usdc_usd,
							"USDT" => &client.aggregator_contracts.chainlink_usdt_usd,
							"DAI" => &client.aggregator_contracts.chainlink_dai_usd,
							"BTC" => &client.aggregator_contracts.chainlink_btc_usd,
							"WBTC" => &client.aggregator_contracts.chainlink_wbtc_usd,
							"CBBTC" => &client.aggregator_contracts.chainlink_cbbtc_usd,
							_ => return Err(eyre!("Invalid symbol")),
						} {
							let (_, price, _, _, _) =
								contract.latestRoundData().call().await?.into();
							let price = price.into_raw();
							let decimals = contract.decimals().call().await?;

							let price = price * U256::from(10u128.pow((18 - decimals).into()));
							let volume = Some(U256::from(1));

							Ok(PriceResponse { price, volume })
						} else {
							Err(eyre!("Invalid symbol"))
						}
					},
					_ => Err(eyre!("Invalid symbol")),
				}
			},
			_ => Err(eyre!("Client not found")),
		}
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>> {
		let mut ret = BTreeMap::new();

		for symbol in [
			"USDC".to_string(),
			"USDT".to_string(),
			"DAI".to_string(),
			"BTC".to_string(),
			"WBTC".to_string(),
			"CBBTC".to_string(),
		] {
			match self.get_ticker_with_symbol(symbol.clone()).await {
				Ok(ticker) => ret.insert(symbol, ticker),
				Err(_) => continue,
			};
		}

		if ret.is_empty() { Err(eyre!("No tickers found")) } else { Ok(ret) }
	}
}

impl<F, P, N: Network> ChainlinkPriceFetcher<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub async fn new(client: Option<Arc<EthClient<F, P, N>>>) -> Self {
		ChainlinkPriceFetcher { client }
	}
}
