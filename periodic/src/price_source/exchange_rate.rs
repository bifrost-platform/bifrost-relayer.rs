use std::{collections::BTreeMap, fmt::Error, ops::Mul};

use alloy::primitives::U256;
use br_primitives::periodic::PriceResponse;

use crate::{price_source::FetchMode, traits::PriceFetcher};

/// Generic currency to USD conversion using ExchangeRatePriceFetcher logic
pub async fn convert_currency_to_usd(currency: &str, amount: U256) -> Result<U256, Error> {
	let primary_url = format!(
		"https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies/{}.min.json",
		currency
	);
	let fallback_url =
		format!("https://latest.currency-api.pages.dev/v1/currencies/{}.min.json", currency);

	// Try primary URL first, fallback to secondary if it fails
	let exchange_rate = match fetch_exchange_rate(&primary_url, currency).await {
		Ok(rate) => rate,
		Err(_) => fetch_exchange_rate(&fallback_url, currency).await?,
	};

	convert_amount_with_rate(amount, exchange_rate)
}

/// Fetch exchange rate from API endpoint
async fn fetch_exchange_rate(url: &str, currency: &str) -> Result<f64, Error> {
	let response = reqwest::get(url).await.map_err(|_| Error)?;
	let data: serde_json::Value = response.json().await.map_err(|_| Error)?;

	// The response structure is: {"date": "...", "jpy": {"usd": 0.00xxx, ...}}
	// We need to extract the currency object and then get the "usd" field
	data.get(currency)
		.and_then(|currency_obj| currency_obj.get("usd"))
		.and_then(|usd_value| usd_value.as_f64())
		.ok_or(Error)
}

/// Convert amount using exchange rate with proper decimal handling
fn convert_amount_with_rate(amount: U256, exchange_rate: f64) -> Result<U256, Error> {
	let exchange_rate_decimal: u32 = {
		let rate_str = exchange_rate.to_string();
		if let Some(decimal_index) = rate_str.find('.') {
			(rate_str.len() - decimal_index - 1) as u32
		} else {
			0
		}
	};

	let scaled_rate =
		U256::from(exchange_rate.mul((10u64.pow(exchange_rate_decimal)) as f64) as u64);

	amount
		.mul(scaled_rate)
		.checked_div(U256::from(10u64.pow(exchange_rate_decimal)))
		.ok_or(Error)
}

#[derive(Clone)]
pub struct ExchangeRatePriceFetcher {
	/// Supported coins and their corresponding currency. (e.g. "JPYC" -> "jpy")
	pub supported_coins: BTreeMap<String, String>,
	/// The mode for fetching prices.
	pub mode: FetchMode,
}

#[async_trait::async_trait]
impl PriceFetcher for ExchangeRatePriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> eyre::Result<PriceResponse> {
		let currency = self.get_currency_from_symbol(&symbol);

		// Use the shared conversion function to get the price of 1 unit (10^18) in USD
		let one_unit = U256::from(10u64.pow(18));
		let price = convert_currency_to_usd(currency, one_unit)
			.await
			.map_err(|_| eyre::eyre!("Failed to convert {} to USD", currency))?;

		Ok(PriceResponse { price, volume: None })
	}

	async fn get_tickers(&self) -> eyre::Result<BTreeMap<String, PriceResponse>> {
		let mut ret = BTreeMap::new();
		match &self.mode {
			FetchMode::Standard => {
				for (symbol, _currency) in &self.supported_coins {
					ret.insert(symbol.clone(), self.get_ticker_with_symbol(symbol.clone()).await?);
				}
			},
			FetchMode::Dedicated(symbol) => {
				ret.insert(symbol.clone(), self.get_ticker_with_symbol(symbol.clone()).await?);
			},
		}
		Ok(ret)
	}
}

impl ExchangeRatePriceFetcher {
	pub fn new(mode: FetchMode) -> Self {
		let supported_coins = BTreeMap::from([("JPYC".into(), "jpy".into())]);

		Self { supported_coins, mode }
	}

	fn get_currency_from_symbol(&self, symbol: &str) -> &str {
		self.supported_coins
			.get(symbol)
			.unwrap_or_else(|| panic!("Cannot find symbol {} in supported coins", symbol))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_get_ticker_with_symbol() {
		let fetcher = ExchangeRatePriceFetcher::new(FetchMode::Standard);
		let res = fetcher
			.get_ticker_with_symbol("JPYC".to_string())
			.await
			.map_err(|e| eyre::eyre!("Failed to get ticker with symbol: {}", e))
			.unwrap();
		println!("Price: {}", res.price);
	}

	#[tokio::test]
	async fn test_get_tickers() {
		let fetcher = ExchangeRatePriceFetcher::new(FetchMode::Standard);
		let res = fetcher
			.get_tickers()
			.await
			.map_err(|e| eyre::eyre!("Failed to get tickers: {}", e))
			.unwrap();
		println!("Tickers: {:?}", res);
	}
}
