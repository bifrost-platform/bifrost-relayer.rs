use std::{collections::BTreeMap, fmt::Error, marker::PhantomData};

use ethers::{providers::JsonRpcClient, utils::parse_ether};
use reqwest::{Response, Url};
use serde::Deserialize;
use tokio::time::{sleep, Duration};

use br_primitives::{periodic::PriceResponse, utils::sub_display_format};

use crate::{price_source::LOG_TARGET, traits::PriceFetcher};

const SUB_LOG_TARGET: &str = "coingecko";

#[derive(Debug, Clone, Deserialize)]
pub struct SupportedCoin {
	pub id: String,
	pub symbol: String,
	pub name: String,
}

#[derive(Clone)]
pub struct CoingeckoPriceFetcher<T> {
	pub base_url: Url,
	pub ids: Vec<String>,
	pub supported_coins: Vec<SupportedCoin>,
	_phantom: PhantomData<T>,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PriceFetcher for CoingeckoPriceFetcher<T> {
	async fn get_ticker_with_symbol(&self, symbol: String) -> Result<PriceResponse, Error> {
		let id = self.get_id_from_symbol(&symbol);
		let url = self
			.base_url
			.join(&format!("simple/price?ids={}&vs_currencies=usd", id))
			.unwrap();

		let price = *self
			._send_request(url)
			.await
			.unwrap()
			.get(id)
			.expect("Cannot find symbol in response")
			.get("usd")
			.expect("Cannot find usd price in response");

		Ok(PriceResponse { price: parse_ether(price).unwrap(), volume: None })
	}

	async fn get_tickers(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		let url = self
			.base_url
			.join(&format!("simple/price?ids={}&vs_currencies=usd", self.ids.join(",")))
			.unwrap();

		return match self._send_request(url).await {
			Ok(response) => {
				let mut ret = BTreeMap::new();
				self.ids.iter().for_each(|id| {
					let price = response.get(id).unwrap().get("usd").unwrap();
					let symbol = self
						.supported_coins
						.iter()
						.find(|coin| coin.id == *id)
						.unwrap()
						.symbol
						.to_uppercase();
					ret.insert(
						symbol,
						PriceResponse { price: parse_ether(price).unwrap(), volume: None },
					);
				});
				Ok(ret)
			},
			Err(e) => Err(e),
		};
	}
}

impl<T: JsonRpcClient> CoingeckoPriceFetcher<T> {
	pub async fn new() -> Result<Self, Error> {
		let ids: Vec<String> = vec![
			"ethereum".into(),
			"bifrost".into(),
			"binancecoin".into(),
			"matic-network".into(),
			"usd-coin".into(),
			"bifi".into(),
			"tether".into(),
			"dai".into(),
			"bitcoin".into(),
			"wrapped-bitcoin".into(),
			"binance-bitcoin".into(),
		];

		let support_coin_list: Vec<SupportedCoin> = Self::get_all_coin_list()
			.await?
			.into_iter()
			.filter(|coin| ids.contains(&coin.id))
			.collect();

		Ok(Self {
			base_url: Url::parse("https://api.coingecko.com/api/v3/").unwrap(),
			ids,
			supported_coins: support_coin_list,
			_phantom: PhantomData,
		})
	}

	async fn get_all_coin_list() -> Result<Vec<SupportedCoin>, Error> {
		let retry_interval = Duration::from_secs(60);
		let mut retries_remaining = 2u8;

		loop {
			match reqwest::get("https://api.coingecko.com/api/v3/coins/list")
				.await
				.and_then(Response::error_for_status)
			{
				Ok(response) => {
					return match response.json::<Vec<SupportedCoin>>().await {
						Ok(coins) => Ok(coins),
						Err(e) => {
							log::error!(
								target: LOG_TARGET,
								"-[{}] ❗️ Error decoding support coin list: {}, Retry in {:?} secs...",
								sub_display_format(SUB_LOG_TARGET),
								e.to_string(),
								retry_interval
							);
							Err(Error)
						},
					}
				},
				Err(e) => {
					log::warn!(
						target: LOG_TARGET,
						"-[{}] ❗️ Error fetching support coin list: {}, Retry in {:?} secs... Retries left: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						e.to_string(),
						retry_interval,
						retries_remaining,
					);
					sentry::capture_error(&e);
					sleep(retry_interval).await;
					retries_remaining -= 1;
				},
			}
		}
	}

	async fn _send_request(
		&self,
		url: Url,
	) -> Result<BTreeMap<String, BTreeMap<String, f64>>, Error> {
		let retry_interval = Duration::from_secs(60);
		let mut retries_remaining = 2u8;

		loop {
			match reqwest::get(url.clone()).await.and_then(Response::error_for_status) {
				Ok(response) => {
					return match response.json::<BTreeMap<String, BTreeMap<String, f64>>>().await {
						Ok(result) => Ok(result),
						Err(e) => {
							log::error!(
								target: LOG_TARGET,
								"-[{}] ❗️ Error decoding coingecko response: {}, retry in secondary sources",
								sub_display_format(SUB_LOG_TARGET),
								e.to_string(),
							);
							Err(Error)
						},
					}
				},
				Err(e) => {
					if retries_remaining == 0 {
						return Err(Error);
					}

					log::warn!(
						target: LOG_TARGET,
						"-[{}] ❗️ Error fetching from coingecko: {}, Retry in {:?} secs... Retries left: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						e.to_string(),
						retry_interval,
						retries_remaining,
					);
					sentry::capture_error(&e);
					sleep(retry_interval).await;
					retries_remaining -= 1;
				},
			}
		}
	}

	fn get_id_from_symbol(&self, symbol: &str) -> &str {
		self.supported_coins
			.iter()
			.find(|coin| coin.symbol == symbol.to_lowercase())
			.expect("Cannot find symbol in support coin list")
			.id
			.as_str()
	}
}
