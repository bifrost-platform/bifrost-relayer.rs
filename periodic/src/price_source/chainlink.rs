use std::{collections::BTreeMap, sync::Arc};

use crate::traits::PriceFetcher;
use alloy::{
	network::AnyNetwork,
	primitives::U256,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	transports::Transport,
};
use br_client::eth::EthClient;
use br_primitives::periodic::PriceResponse;
use eyre::{eyre, Result};

#[derive(Clone)]
pub struct ChainlinkPriceFetcher<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	client: Option<Arc<EthClient<F, P, T>>>,
}

#[async_trait::async_trait]
impl<F, P, T> PriceFetcher for ChainlinkPriceFetcher<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Get the price of a symbol from Chainlink aggregator.
	/// Available symbols: USDC, USDT, DAI, BTC, WBTC, CBBTC
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse> {
		if let Some(client) = &self.client {
			let symbol_str = symbol.as_str();

			match symbol_str {
				"USDC" | "USDT" | "DAI" | "BTC" | "WBTC" | "CBBTC" => {
					return if let Some(contract) = match symbol_str {
						"USDC" => &client.aggregator_contracts.chainlink_usdc_usd,
						"USDT" => &client.aggregator_contracts.chainlink_usdt_usd,
						"DAI" => &client.aggregator_contracts.chainlink_dai_usd,
						"BTC" => &client.aggregator_contracts.chainlink_btc_usd,
						"WBTC" => &client.aggregator_contracts.chainlink_wbtc_usd,
						"CBBTC" => &client.aggregator_contracts.chainlink_cbbtc_usd,
						_ => return Err(eyre!("Invalid symbol")),
					} {
						let (_, price, _, _, _) = contract.latestRoundData().call().await?.into();
						let price = price.into_raw();
						let decimals = contract.decimals().call().await?._0;

						let price = price * U256::from(10u128.pow((18 - decimals).into()));
						let volume = Some(U256::from(1));

						Ok(PriceResponse { price, volume })
					} else {
						Err(eyre!("Invalid symbol"))
					}
				},
				_ => {
					return Err(eyre!("Invalid symbol"));
				},
			}
		} else {
			return Err(eyre!("Client not found"));
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

		return if ret.is_empty() { Err(eyre!("No tickers found")) } else { Ok(ret) };
	}
}

impl<F, P, T> ChainlinkPriceFetcher<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	pub async fn new(client: Option<Arc<EthClient<F, P, T>>>) -> Self {
		ChainlinkPriceFetcher { client }
	}
}
