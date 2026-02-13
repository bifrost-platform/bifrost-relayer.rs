use std::{collections::BTreeMap, fmt::Error, str::FromStr, sync::Arc};

use alloy::{
	network::Network,
	primitives::{B256, FixedBytes, U256, utils::parse_ether},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cron::Schedule;
use eyre::Result;
use rand::Rng;
use sc_service::SpawnTaskHandle;
use tokio::time::sleep;

use br_client::eth::{ClientMap, EthClient, send_transaction};
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PRICE_FEEDER_SCHEDULE},
	contracts::socket::get_asset_oids,
	eth::AggregatorContracts,
	periodic::{PriceResponse, PriceSource},
	tx::{PriceFeedMetadata, TxRequestMetadata},
	utils::sub_display_format,
};

use crate::{
	price_source::{FetchMode, PriceFetchers},
	traits::{PeriodicWorker, PriceFetcher},
};

const SUB_LOG_TARGET: &str = "price-feeder";

/// The essential task that handles oracle price feedings.
pub struct OraclePriceFeeder<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<F, P, N>>,
	/// The time schedule that represents when to send price feeds.
	schedule: Schedule,
	/// The primary sources for fetching prices. (Standard: Coingecko, Dedicated)
	/// In cases when token volume is too low in the exchanges, we have to use the dedicated source.
	primary_sources: Vec<PriceFetchers<F, P, N>>,
	/// The secondary sources for fetching prices. (aggregated from external sources)
	secondary_sources: Vec<PriceFetchers<F, P, N>>,
	/// The pre-defined oracle ID's for each asset.
	asset_oid: BTreeMap<&'static str, B256>,
	/// The vector that contains each `EthClient`.
	clients: Arc<ClientMap<F, P, N>>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

#[async_trait]
impl<F, P, N: Network> PeriodicWorker for OraclePriceFeeder<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		self.initialize_fetchers().await;

		loop {
			let upcoming = self.schedule.upcoming(Utc).next().unwrap();
			self.feed_period_spreader(upcoming, true).await;

			if self.client.is_selected_relayer().await? {
				if self.primary_sources.is_empty() {
					panic!("Primary source is empty");
				} else {
					self.try_with_primary().await;
				}
			}

			self.feed_period_spreader(upcoming, false).await;
		}
	}
}

impl<F, P, N: Network> OraclePriceFeeder<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	pub fn new(
		client: Arc<EthClient<F, P, N>>,
		clients: Arc<ClientMap<F, P, N>>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
	) -> Self {
		let asset_oid = get_asset_oids();

		Self {
			schedule: Schedule::from_str(PRICE_FEEDER_SCHEDULE).expect(INVALID_PERIODIC_SCHEDULE),
			primary_sources: vec![],
			secondary_sources: vec![],
			asset_oid,
			client,
			clients,
			handle,
			debug_mode,
		}
	}

	async fn feed_period_spreader(&self, until: DateTime<Utc>, in_between: bool) {
		let should_be_done_in = until - Utc::now();

		if in_between {
			let sleep_duration = should_be_done_in
				- chrono::Duration::seconds(
					rand::rng().random_range(0..=should_be_done_in.num_seconds()),
				);

			if let Ok(sleep_duration) = sleep_duration.to_std() {
				sleep(sleep_duration).await
			}
		} else if let Ok(sleep_duration) = should_be_done_in.to_std() {
			sleep(sleep_duration).await
		}
	}

	async fn try_with_primary(&self) {
		let mut all_prices = BTreeMap::new();
		let mut failed_modes = Vec::new();

		// Collect prices from all primary sources
		for fetcher in &self.primary_sources {
			match fetcher.get_tickers().await {
				Ok(price_responses) => {
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] ✅ Fetched {} prices from primary source (mode: {:?})",
						sub_display_format(SUB_LOG_TARGET),
						price_responses.len(),
						fetcher.mode()
					);
					all_prices.extend(price_responses);
				},
				Err(_) => {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚠️  Failed to fetch price feed data from primary source (mode: {:?}). Will retry with secondary sources.",
						sub_display_format(SUB_LOG_TARGET),
						fetcher.mode()
					);
					failed_modes.push(fetcher.mode());
				},
			};
		}

		// Fetch from secondary sources for failed modes
		for mode in failed_modes {
			match self.fetch_from_secondary(mode.clone()).await {
				Ok(secondary_prices) => {
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] ✅ Fetched {} prices from secondary sources (mode: {:?})",
						sub_display_format(SUB_LOG_TARGET),
						secondary_prices.len(),
						mode
					);
					all_prices.extend(secondary_prices);
				},
				Err(_) => {
					br_primitives::log_and_capture!(
						error,
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address().await,
						"❗️ Failed to fetch prices from secondary sources (mode: {:?}). Skipping this feeding.",
						mode
					);
				},
			}
		}

		// Send single transaction with all collected prices
		if all_prices.is_empty() {
			br_primitives::log_and_capture!(
				error,
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address().await,
				"❗️ Failed to fetch any price feed data from all sources. Skipping this feeding."
			);
		} else {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 📦 Sending single transaction with {} prices",
				sub_display_format(SUB_LOG_TARGET),
				all_prices.len()
			);
			self.build_and_send_transaction(all_prices).await;
		}
	}

	/// If price data fetch failed with primary source, try with secondary sources.
	async fn fetch_from_secondary(
		&self,
		mode: FetchMode,
	) -> Result<BTreeMap<String, PriceResponse>, Error> {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] Fetching with secondary sources, mode: {:?}",
			sub_display_format(SUB_LOG_TARGET),
			mode
		);

		// (volume weighted price sum, total volume)
		let mut volume_weighted: BTreeMap<String, (U256, U256)> = BTreeMap::new();

		for fetcher in &self.secondary_sources {
			if fetcher.mode() != mode {
				continue;
			}
			match fetcher.get_tickers().await {
				Ok(tickers) => {
					tickers.iter().for_each(|(symbol, price_response)| {
						if !price_response.volume.unwrap().is_zero() {
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
						}
					});
				},
				Err(_) => continue,
			};
		}

		if volume_weighted.is_empty() {
			return Err(Error);
		}

		match mode {
			FetchMode::Standard => {
				if !volume_weighted.contains_key("USDC") {
					volume_weighted
						.insert("USDC".into(), (parse_ether("1").unwrap(), U256::from(1)));
				}
				if !volume_weighted.contains_key("USDT") {
					volume_weighted
						.insert("USDT".into(), (parse_ether("1").unwrap(), U256::from(1)));
				}
				if !volume_weighted.contains_key("DAI") {
					volume_weighted
						.insert("DAI".into(), (parse_ether("1").unwrap(), U256::from(1)));
				}
			},
			FetchMode::Dedicated(_) => {},
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
		if let Ok(primary) =
			PriceFetchers::new(PriceSource::Coingecko, None, FetchMode::Standard).await
		{
			self.primary_sources.push(primary);
		}
		// for testnet only
		// TODO: remove this when mainnet is ready
		if self.client.chain_id() == 49088 {
			if let Ok(primary) = PriceFetchers::new(
				PriceSource::ExchangeRate,
				None,
				FetchMode::Dedicated("JPYC".into()),
			)
			.await
			{
				self.primary_sources.push(primary);
			}
		}

		let secondary_sources = vec![
			PriceSource::Binance,
			PriceSource::Gateio,
			PriceSource::Kucoin,
			PriceSource::Upbit,
		];
		for source in secondary_sources {
			if let Ok(fetcher) = PriceFetchers::new(source, None, FetchMode::Standard).await {
				self.secondary_sources.push(fetcher);
			}
		}

		for (_, client) in self.clients.iter() {
			if Self::has_any_chainlink_feeds(&client.aggregator_contracts) {
				if let Ok(fetcher) = PriceFetchers::new(
					PriceSource::Chainlink,
					client.clone().into(),
					FetchMode::Standard,
				)
				.await
				{
					self.secondary_sources.push(fetcher);
				}
			}
			if Self::has_chainlink_feed(&client.aggregator_contracts, "JPYC") {
				if let Ok(fetcher) = PriceFetchers::new(
					PriceSource::Chainlink,
					client.clone().into(),
					FetchMode::Dedicated("JPYC".into()),
				)
				.await
				{
					self.secondary_sources.push(fetcher);
				}
			}
		}
	}

	fn has_any_chainlink_feeds(contracts: &AggregatorContracts<F, P, N>) -> bool {
		[
			&contracts.chainlink_usdc_usd,
			&contracts.chainlink_usdt_usd,
			&contracts.chainlink_dai_usd,
			&contracts.chainlink_btc_usd,
			&contracts.chainlink_wbtc_usd,
			&contracts.chainlink_cbbtc_usd,
		]
		.iter()
		.any(|contract| contract.is_some())
	}

	fn has_chainlink_feed(contracts: &AggregatorContracts<F, P, N>, symbol: &str) -> bool {
		match symbol {
			"USDC" => contracts.chainlink_usdc_usd.is_some(),
			"USDT" => contracts.chainlink_usdt_usd.is_some(),
			"DAI" => contracts.chainlink_dai_usd.is_some(),
			"BTC" => contracts.chainlink_btc_usd.is_some(),
			"WBTC" => contracts.chainlink_wbtc_usd.is_some(),
			"CBBTC" => contracts.chainlink_cbbtc_usd.is_some(),
			"JPYC" => contracts.chainlink_jpy_usd.is_some(),
			_ => false,
		}
	}

	/// Build and send transaction.
	async fn build_and_send_transaction(&self, price_responses: BTreeMap<String, PriceResponse>) {
		let mut oid_bytes_list: Vec<FixedBytes<32>> = vec![];
		let mut price_bytes_list: Vec<FixedBytes<32>> = vec![];

		price_responses.iter().for_each(|(symbol, price_response)| {
			oid_bytes_list.push(*self.asset_oid.get(symbol.as_str()).unwrap());
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
		oid_bytes_list: Vec<FixedBytes<32>>,
		price_bytes_list: Vec<FixedBytes<32>>,
	) -> N::TransactionRequest {
		self.client
			.protocol_contracts
			.socket
			.oracle_aggregate_feeding(oid_bytes_list, price_bytes_list)
			.into_transaction_request()
	}

	/// Request send transaction to the target tx request channel.
	async fn request_send_transaction(
		&self,
		tx_request: N::TransactionRequest,
		metadata: PriceFeedMetadata,
	) {
		send_transaction(
			self.client.clone(),
			tx_request,
			SUB_LOG_TARGET.to_string(),
			TxRequestMetadata::PriceFeed(metadata),
			self.debug_mode,
			self.handle.clone(),
		);
	}
}
