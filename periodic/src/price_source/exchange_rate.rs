use std::{collections::BTreeMap, fmt::Error};

use alloy::primitives::U256;
use br_primitives::periodic::PriceResponse;
use reqwest::Client;

use crate::{price_source::FetchMode, traits::PriceFetcher};

/// Generic currency to USD conversion using ExchangeRatePriceFetcher logic
pub async fn convert_currency_to_usd(
	client: &Client,
	currency: &str,
	amount: U256,
) -> Result<U256, Error> {
	let primary_url = format!(
		"https://cdn.jsdelivr.net/npm/@fawazahmed0/currency-api@latest/v1/currencies/{}.min.json",
		currency
	);
	let fallback_url =
		format!("https://latest.currency-api.pages.dev/v1/currencies/{}.min.json", currency);

	// Try primary URL first, fallback to secondary if it fails
	let exchange_rate = match fetch_exchange_rate(client, &primary_url, currency).await {
		Ok(rate) => rate,
		Err(_) => fetch_exchange_rate(client, &fallback_url, currency).await?,
	};

	convert_amount_with_rate(amount, &exchange_rate)
}

/// Fetch exchange rate from API endpoint as a string to avoid f64 precision loss.
async fn fetch_exchange_rate(client: &Client, url: &str, currency: &str) -> Result<String, Error> {
	let response = client.get(url).send().await.map_err(|_| Error)?;
	let data: serde_json::Value = response.json().await.map_err(|_| Error)?;

	data.get(currency)
		.and_then(|currency_obj| currency_obj.get("usd"))
		.filter(|v| v.is_number())
		.map(|v| v.to_string())
		.ok_or(Error)
}

/// Normalize a scientific notation string (e.g. "1.23e-5") into a plain decimal string
/// (e.g. "0.0000123"). Returns the input unchanged if no exponent is present.
fn normalize_scientific_notation(s: &str) -> String {
	let (mantissa, exponent) = match s.find(|c: char| c == 'e' || c == 'E') {
		Some(pos) => (&s[..pos], s[pos + 1..].parse::<i32>().unwrap_or(0)),
		None => return s.to_string(),
	};

	let (integer, fractional) = if let Some(dot_pos) = mantissa.find('.') {
		(&mantissa[..dot_pos], &mantissa[dot_pos + 1..])
	} else {
		(mantissa, "")
	};

	// digits = all significant digits without the dot
	let digits = format!("{}{}", integer.trim_start_matches('0'), fractional);
	// decimal_pos = where the decimal point sits relative to the start of digits
	let decimal_pos = integer.trim_start_matches('0').len() as i32 + exponent;

	if decimal_pos <= 0 {
		format!("0.{}{}", "0".repeat((-decimal_pos) as usize), digits)
	} else if (decimal_pos as usize) >= digits.len() {
		format!("{}{}", digits, "0".repeat(decimal_pos as usize - digits.len()))
	} else {
		let (left, right) = digits.split_at(decimal_pos as usize);
		format!("{}.{}", left, right)
	}
}

/// Parse a decimal rate string (e.g. "0.006734") into U256 scaled by 10^18,
/// without any f64 arithmetic. Also handles scientific notation (e.g. "1.23e-5").
fn rate_to_scaled_u256(rate_str: &str) -> Result<U256, Error> {
	const DECIMALS: usize = 18;

	let normalized = normalize_scientific_notation(rate_str);
	let (integer_part, fractional_part) = if let Some(dot_pos) = normalized.find('.') {
		(&normalized[..dot_pos], &normalized[dot_pos + 1..])
	} else {
		(normalized.as_str(), "")
	};

	let combined = if fractional_part.len() >= DECIMALS {
		format!("{}{}", integer_part, &fractional_part[..DECIMALS])
	} else {
		format!(
			"{}{}{}",
			integer_part,
			fractional_part,
			"0".repeat(DECIMALS - fractional_part.len())
		)
	};

	let trimmed = combined.trim_start_matches('0');
	if trimmed.is_empty() {
		return Ok(U256::ZERO);
	}

	U256::from_str_radix(trimmed, 10).map_err(|_| Error)
}

/// Convert amount using exchange rate string with proper decimal handling.
fn convert_amount_with_rate(amount: U256, rate_str: &str) -> Result<U256, Error> {
	let rate_scaled = rate_to_scaled_u256(rate_str)?;

	amount
		.checked_mul(rate_scaled)
		.and_then(|v| v.checked_div(U256::from(10u64.pow(18))))
		.ok_or(Error)
}

#[derive(Clone)]
pub struct ExchangeRatePriceFetcher {
	/// Supported coins and their corresponding currency. (e.g. "JPYC" -> "jpy")
	pub supported_coins: BTreeMap<String, String>,
	/// The shared HTTP client.
	client: Client,
	/// The mode for fetching prices.
	pub mode: FetchMode,
}

#[async_trait::async_trait]
impl PriceFetcher for ExchangeRatePriceFetcher {
	async fn get_ticker_with_symbol(&self, symbol: String) -> eyre::Result<PriceResponse> {
		let currency = self.get_currency_from_symbol(&symbol);

		// Use the shared conversion function to get the price of 1 unit (10^18) in USD
		let one_unit = U256::from(10u64.pow(18));
		let price = convert_currency_to_usd(&self.client, currency, one_unit)
			.await
			.map_err(|_| eyre::eyre!("Failed to convert {} to USD", currency))?;

		Ok(PriceResponse { price, volume: U256::ZERO })
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
	pub fn new(client: Client, mode: FetchMode) -> Self {
		let supported_coins = BTreeMap::from([("JPYC".into(), "jpy".into())]);

		Self { supported_coins, client, mode }
	}

	fn get_currency_from_symbol(&self, symbol: &str) -> &str {
		self.supported_coins
			.get(symbol)
			.unwrap_or_else(|| panic!("Cannot find symbol {} in supported coins", symbol))
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn test_client() -> Client {
		Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
	}

	#[tokio::test]
	async fn test_get_ticker_with_symbol() {
		let fetcher = ExchangeRatePriceFetcher::new(test_client(), FetchMode::Standard);
		let res = fetcher
			.get_ticker_with_symbol("JPYC".to_string())
			.await
			.map_err(|e| eyre::eyre!("Failed to get ticker with symbol: {}", e))
			.unwrap();
		println!("Price: {}", res.price);
	}

	#[tokio::test]
	async fn test_get_tickers() {
		let fetcher = ExchangeRatePriceFetcher::new(test_client(), FetchMode::Standard);
		let res = fetcher
			.get_tickers()
			.await
			.map_err(|e| eyre::eyre!("Failed to get tickers: {}", e))
			.unwrap();
		println!("Tickers: {:?}", res);
	}

	#[test]
	fn test_normalize_scientific_notation() {
		assert_eq!(normalize_scientific_notation("1.23e-5"), "0.0000123");
		assert_eq!(normalize_scientific_notation("5e-3"), "0.005");
		assert_eq!(normalize_scientific_notation("1.5e3"), "1500");
		assert_eq!(normalize_scientific_notation("1.23e0"), "1.23");
		assert_eq!(normalize_scientific_notation("0.006734"), "0.006734");
		assert_eq!(normalize_scientific_notation("123"), "123");
	}

	#[test]
	fn test_rate_to_scaled_u256_scientific() {
		// 1.23e-5 = 0.0000123 → 12300000000000 (1.23e-5 * 1e18)
		let result = rate_to_scaled_u256("1.23e-5").unwrap();
		assert_eq!(result, U256::from(12_300_000_000_000u64));

		// Normal decimal should still work
		let result = rate_to_scaled_u256("0.006734").unwrap();
		assert_eq!(result, U256::from(6_734_000_000_000_000u64));
	}
}
