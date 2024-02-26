use std::{collections::BTreeMap, fmt::Error, sync::Arc};

use br_primitives::periodic::PriceResponse;
use ethers::{providers::JsonRpcClient, types::U256};

use br_client::eth::EthClient;

use crate::traits::PriceFetcher;

#[derive(Clone)]
pub struct ChainlinkPriceFetcher<T> {
	client: Option<Arc<EthClient<T>>>,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static> PriceFetcher for ChainlinkPriceFetcher<T> {
	/// Should use only when requesting USDC/USDT/DAI (stable coins) price.
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse, Error> {
		if let Some(client) = &self.client {
			let symbol_str = symbol.as_str();

			match symbol_str {
				"USDC" | "USDT" | "DAI" | "BTC" | "WBTC" => {
					return if let Some(contract) = match symbol_str {
						"USDC" => &client.aggregator_contracts.chainlink_usdc_usd,
						"USDT" => &client.aggregator_contracts.chainlink_usdt_usd,
						"DAI" => &client.aggregator_contracts.chainlink_dai_usd,
						"BTC" => &client.aggregator_contracts.chainlink_btc_usd,
						"WBTC" => &client.aggregator_contracts.chainlink_wbtc_usd,
						_ => todo!(),
					} {
						let (_, price, _, _, _) = contract.latest_round_data().await.unwrap();
						let decimals = contract.decimals().await.unwrap();

						Ok(PriceResponse {
							price: U256::from(
								(price * 10u128.pow((18 - decimals).into())).as_u128(),
							),
							volume: U256::from(1).into(),
						})
					} else {
						Err(Error)
					}
				},
				_ => {
					todo!()
				},
			}
		} else {
			return Err(Error);
		}
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		let mut ret = BTreeMap::new();

		for symbol in ["USDC".to_string(), "USDT".to_string(), "DAI".to_string(), "BTC".to_string()]
		{
			match self.get_ticker_with_symbol(symbol.clone()).await {
				Ok(ticker) => ret.insert(symbol, ticker),
				Err(_) => continue,
			};
		}

		return if ret.is_empty() { Err(Error) } else { Ok(ret) };
	}
}

impl<T: JsonRpcClient> ChainlinkPriceFetcher<T> {
	pub async fn new(client: Option<Arc<EthClient<T>>>) -> Self {
		ChainlinkPriceFetcher { client }
	}
}
