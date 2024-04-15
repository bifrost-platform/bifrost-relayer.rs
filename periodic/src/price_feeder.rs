use std::{collections::BTreeMap, fmt::Error, str::FromStr, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cron::Schedule;
use ethers::{
	providers::JsonRpcClient,
	types::{TransactionRequest, H256, U256},
	utils::parse_ether,
};
use rand::Rng;
use tokio::time::sleep;

use br_client::eth::EthClient;
use br_primitives::{
	constants::{
		errors::{INVALID_BIFROST_NATIVENESS, INVALID_PERIODIC_SCHEDULE},
		schedule::PRICE_FEEDER_SCHEDULE,
	},
	contracts::socket::get_asset_oids,
	eth::GasCoefficient,
	periodic::{PriceResponse, PriceSource},
	tx::{PriceFeedMetadata, TxRequest, TxRequestMessage, TxRequestMetadata, TxRequestSender},
	utils::sub_display_format,
};

use crate::{
	price_source::PriceFetchers,
	traits::{PeriodicWorker, PriceFetcher},
};

const SUB_LOG_TARGET: &str = "price-feeder";

/// The essential task that handles oracle price feedings.
pub struct OraclePriceFeeder<T> {
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<T>>,
	/// The time schedule that represents when to send price feeds.
	schedule: Schedule,
	/// The primary source for fetching prices. (Coingecko)
	primary_source: Vec<PriceFetchers<T>>,
	/// The secondary source for fetching prices. (aggregated from external sources)
	secondary_sources: Vec<PriceFetchers<T>>,
	/// The sender that sends messages to the tx request channel.
	tx_request_sender: Arc<TxRequestSender>,
	/// The pre-defined oracle ID's for each asset.
	asset_oid: BTreeMap<&'static str, H256>,
	/// The vector that contains each `EthClient`.
	system_clients: Vec<Arc<EthClient<T>>>,
}

#[async_trait]
impl<T: JsonRpcClient + 'static> PeriodicWorker for OraclePriceFeeder<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		self.initialize_fetchers().await;

		loop {
			let upcoming = self.schedule.upcoming(Utc).next().unwrap();
			self.feed_period_spreader(upcoming, true).await;

			if self.client.is_selected_relayer().await {
				if self.primary_source.is_empty() {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚠️  Failed to initialize primary fetcher. Trying to fetch with secondary sources.",
						sub_display_format(SUB_LOG_TARGET),
					);
					self.try_with_secondary().await;
				} else {
					self.try_with_primary().await;
				}
			}

			self.feed_period_spreader(upcoming, false).await;
		}
	}
}

impl<T: JsonRpcClient + 'static> OraclePriceFeeder<T> {
	pub fn new(
		tx_request_senders: Vec<Arc<TxRequestSender>>,
		system_clients: Vec<Arc<EthClient<T>>>,
	) -> Self {
		let asset_oid = get_asset_oids();

		Self {
			schedule: Schedule::from_str(PRICE_FEEDER_SCHEDULE).expect(INVALID_PERIODIC_SCHEDULE),
			primary_source: vec![],
			secondary_sources: vec![],
			tx_request_sender: tx_request_senders
				.iter()
				.find(|sender| sender.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
			asset_oid,
			client: system_clients
				.iter()
				.find(|client| client.metadata.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
			system_clients,
		}
	}

	async fn feed_period_spreader(&self, until: DateTime<Utc>, in_between: bool) {
		let should_be_done_in = until - Utc::now();

		if in_between {
			let sleep_duration = should_be_done_in
				- chrono::Duration::seconds(
					rand::thread_rng().gen_range(0..=should_be_done_in.num_seconds()),
				);

			if let Ok(sleep_duration) = sleep_duration.to_std() {
				sleep(sleep_duration).await
			}
		} else if let Ok(sleep_duration) = should_be_done_in.to_std() {
			sleep(sleep_duration).await
		}
	}

	async fn feed_period_spreader(&self, until: DateTime<Utc>, in_between: bool) {
		let should_be_done_in = until - Utc::now();

		if in_between {
			let sleep_duration = should_be_done_in
				- chrono::Duration::seconds(
					rand::thread_rng().gen_range(0..=should_be_done_in.num_seconds()),
				);

			if let Ok(sleep_duration) = sleep_duration.to_std() {
				sleep(sleep_duration).await
			}
		} else if let Ok(sleep_duration) = should_be_done_in.to_std() {
			sleep(sleep_duration).await
		}
	}

	async fn try_with_primary(&self) {
		match self.primary_source[0].get_tickers().await {
			// If primary source works well.
			Ok(price_responses) => {
				self.build_and_send_transaction(price_responses).await;
			},
			// If primary source fails.
			Err(_) => {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚠️  Failed to fetch price feed data from the primary source. Retrying to fetch with secondary sources.",
					sub_display_format(SUB_LOG_TARGET),
				);

				self.try_with_secondary().await;
			},
		};
	}

	async fn try_with_secondary(&self) {
		match self.fetch_from_secondary().await {
			Ok(price_responses) => {
				self.build_and_send_transaction(price_responses).await;
			},
			Err(_) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to fetch price feed data from secondary sources. First off, skip this feeding.",
					sub_display_format(SUB_LOG_TARGET),
					self.client.address()
				);
				log::error!(target: &self.client.get_chain_name(), "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", &self.client.get_chain_name()),
					sentry::Level::Error,
				);
			},
		}
	}

	/// If price data fetch failed with primary source, try with secondary sources.
	async fn fetch_from_secondary(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		// (volume weighted price sum, total volume)
		let mut volume_weighted: BTreeMap<String, (U256, U256)> = BTreeMap::new();

		for fetcher in &self.secondary_sources {
			match fetcher.get_tickers().await {
				Ok(tickers) => {
					tickers.iter().for_each(|(symbol, price_response)| {
						if let Some(value) = volume_weighted.get_mut(symbol) {
							value.0 += price_response.price * price_response.volume.unwrap();
							value.1 += price_response.volume.unwrap();
						} else {
							volume_weighted.insert(
								symbol.clone(),
								(
									price_response.price * price_response.volume.unwrap(),
									price_response.volume.unwrap(),
								),
							);
						}
					});
				},
				Err(_) => continue,
			};
		}

		if volume_weighted.is_empty() {
			return Err(Error);
		}

		if !volume_weighted.contains_key("USDC") {
			volume_weighted.insert("USDC".into(), (parse_ether(1).unwrap(), U256::from(1)));
		}
		if !volume_weighted.contains_key("USDT") {
			volume_weighted.insert("USDT".into(), (parse_ether(1).unwrap(), U256::from(1)));
		}
		if !volume_weighted.contains_key("DAI") {
			volume_weighted.insert("DAI".into(), (parse_ether(1).unwrap(), U256::from(1)));
		}

		if !volume_weighted.contains_key("USDC") {
			volume_weighted.insert("USDC".into(), (parse_ether(1).unwrap(), U256::from(1)));
		}
		if !volume_weighted.contains_key("USDT") {
			volume_weighted.insert("USDT".into(), (parse_ether(1).unwrap(), U256::from(1)));
		}
		if !volume_weighted.contains_key("DAI") {
			volume_weighted.insert("DAI".into(), (parse_ether(1).unwrap(), U256::from(1)));
		}

		Ok(volume_weighted
			.into_iter()
			.map(|(symbol, (volume_weighted_sum, total_volume))| {
				(
					symbol,
					PriceResponse {
						price: volume_weighted_sum / total_volume,
						volume: total_volume.into(),
					},
				)
			})
			.collect())
	}

	/// Initialize price fetchers. Can't move into new().
	async fn initialize_fetchers(&mut self) {
		if let Ok(primary) = PriceFetchers::new(PriceSource::Coingecko, None).await {
			self.primary_source.push(primary);
		}

		let secondary_sources = vec![
			PriceSource::Binance,
			PriceSource::Gateio,
			PriceSource::Kucoin,
			PriceSource::Upbit,
		];
		for source in secondary_sources {
			if let Ok(fetcher) = PriceFetchers::new(source, None).await {
				self.secondary_sources.push(fetcher);
			}
		}
		for client in &self.system_clients {
			if client.aggregator_contracts.chainlink_usdc_usd.is_some()
				|| client.aggregator_contracts.chainlink_usdt_usd.is_some()
			{
				if let Ok(fetcher) =
					PriceFetchers::new(PriceSource::Chainlink, client.clone().into()).await
				{
					self.secondary_sources.push(fetcher);
				}
			}
		}

		if volume_weighted.is_empty() {
			return Err(Error::default());
		}

		Ok(volume_weighted
			.into_iter()
			.map(|(symbol, (volume_weighted_sum, total_volume))| {
				(
					symbol,
					PriceResponse {
						price: volume_weighted_sum / total_volume,
						volume: total_volume.into(),
					},
				)
			})
			.collect())
	}

	/// Build and send transaction.
	async fn build_and_send_transaction(&self, price_responses: BTreeMap<String, PriceResponse>) {
		let mut oid_bytes_list: Vec<[u8; 32]> = vec![];
		let mut price_bytes_list: Vec<[u8; 32]> = vec![];

		price_responses.iter().for_each(|(symbol, price_response)| {
			oid_bytes_list.push(self.asset_oid.get(symbol.as_str()).unwrap().to_fixed_bytes());
			price_bytes_list.push(price_response.price.into());
		});

		self.request_send_transaction(
			self.build_transaction(oid_bytes_list, price_bytes_list).await,
			PriceFeedMetadata::new(price_responses),
		)
		.await;
	}

	/// Build price feed transaction.
	async fn build_transaction(
		&self,
		oid_bytes_list: Vec<[u8; 32]>,
		price_bytes_list: Vec<[u8; 32]>,
	) -> TransactionRequest {
		TransactionRequest::default()
			.to(self.client.protocol_contracts.socket.address())
			.data(
				self.client
					.protocol_contracts
					.socket
					.oracle_aggregate_feeding(oid_bytes_list, price_bytes_list)
					.calldata()
					.unwrap(),
			)
	}

	/// Request send transaction to the target tx request channel.
	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: PriceFeedMetadata,
	) {
		match self.tx_request_sender.send(TxRequestMessage::new(
			TxRequest::Legacy(tx_request),
			TxRequestMetadata::PriceFeed(metadata.clone()),
			false,
			false,
			GasCoefficient::Mid,
			false,
			false,
			GasCoefficient::Mid,
			false,
		)) {
			Ok(()) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 💵 Request price feed transaction to chain({:?}): {}",
				sub_display_format(SUB_LOG_TARGET),
				self.client.get_chain_id(),
				metadata
			),
			Err(error) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to request price feed transaction to chain({:?}): {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.client.address(),
					self.client.get_chain_id(),
					metadata,
					error.to_string()
				);
				log::error!(target: &self.client.get_chain_name(), "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", &self.client.get_chain_name()),
					sentry::Level::Error,
				);
			},
		}
	}
}
