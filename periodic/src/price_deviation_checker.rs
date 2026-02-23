use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use alloy::{
	network::Network,
	primitives::{B256, FixedBytes, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use async_trait::async_trait;
use cron::Schedule;
use eyre::Result;
use reqwest::Client;
use sc_service::SpawnTaskHandle;

use br_client::eth::{ClientMap, EthClient, send_transaction};
use br_primitives::{
	constants::{
		config::{DEFAULT_PRICE_DEVIATION_THRESHOLD_BPS, PRICE_FETCHER_REQUEST_TIMEOUT},
		errors::INVALID_PERIODIC_SCHEDULE,
		schedule::PRICE_DEVIATION_CHECK_SCHEDULE,
	},
	contracts::{
		oracle::{OracleManagerContract, OracleManagerInstance},
		socket::get_asset_oids,
	},
	periodic::{PriceResponse, PriceSource},
	tx::{PriceFeedMetadata, TxRequestMetadata},
	utils::sub_display_format,
};

use crate::{
	price_source::{FetchMode, PriceFetchers},
	traits::{PeriodicWorker, PriceFetcher},
};

const SUB_LOG_TARGET: &str = "deviation-checker";

/// Returns the deviation threshold in basis points for a given symbol.
/// Default is `DEFAULT_PRICE_DEVIATION_THRESHOLD_BPS` (2%).
/// Override per symbol as needed.
fn get_deviation_threshold_bps(symbol: &str) -> u64 {
	match symbol {
		_ => DEFAULT_PRICE_DEVIATION_THRESHOLD_BPS,
	}
}

/// The periodic task that monitors price deviation between on-chain oracle
/// and current market prices, triggering an immediate feed when deviation
/// exceeds the per-asset threshold.
pub struct PriceDeviationChecker<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<F, P, N>>,
	/// The time schedule for deviation checks.
	schedule: Schedule,
	/// The dedicated source for a specific token (e.g. ExchangeRate for JPYC).
	dedicated_source: Option<PriceFetchers<F, P, N>>,
	/// The market sources for fetching current prices (VWAP aggregated).
	market_sources: Vec<PriceFetchers<F, P, N>>,
	/// The Oracle Manager contract instance for reading on-chain prices.
	oracle_manager: Option<OracleManagerInstance<F, P, N>>,
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
impl<F, P, N: Network> PeriodicWorker for PriceDeviationChecker<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		self.initialize().await;

		loop {
			self.wait_until_next_time().await;

			if self.client.is_selected_relayer().await? {
				self.check_and_feed().await;
			}
		}
	}
}

impl<F, P, N: Network> PriceDeviationChecker<F, P, N>
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
			schedule: Schedule::from_str(PRICE_DEVIATION_CHECK_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
			dedicated_source: None,
			market_sources: vec![],
			oracle_manager: None,
			asset_oid,
			client,
			clients,
			handle,
			debug_mode,
		}
	}

	/// Initialize market sources and resolve oracle manager address.
	async fn initialize(&mut self) {
		let http_client = Client::builder()
			.timeout(Duration::from_secs(PRICE_FETCHER_REQUEST_TIMEOUT))
			.build()
			.expect("Failed to build HTTP client");

		// Initialize market sources
		let sources = vec![
			PriceSource::Binance,
			PriceSource::Bithumb,
			PriceSource::Gateio,
			PriceSource::Kucoin,
			PriceSource::Upbit,
		];
		for source in sources {
			if let Ok(fetcher) =
				PriceFetchers::new(source, None, http_client.clone(), FetchMode::Standard).await
			{
				self.market_sources.push(fetcher);
			}
		}

		// Initialize dedicated source for JPYC (ExchangeRate)
		if let Ok(fetcher) = PriceFetchers::new(
			PriceSource::ExchangeRate,
			None,
			http_client.clone(),
			FetchMode::Dedicated("JPYC".into()),
		)
		.await
		{
			self.dedicated_source = Some(fetcher);
		}

		// Initialize Chainlink sources
		for (_, client) in self.clients.iter() {
			if let Ok(fetcher) = PriceFetchers::new(
				PriceSource::Chainlink,
				client.clone().into(),
				http_client.clone(),
				FetchMode::Standard,
			)
			.await
			{
				self.market_sources.push(fetcher);
			}
			if client.aggregator_contracts.chainlink_jpy_usd.is_some() {
				if let Ok(fetcher) = PriceFetchers::new(
					PriceSource::Chainlink,
					client.clone().into(),
					http_client.clone(),
					FetchMode::Dedicated("JPYC".into()),
				)
				.await
				{
					self.market_sources.push(fetcher);
				}
			}
		}

		// Resolve Oracle Manager address from socket contract
		match self.client.protocol_contracts.socket.orc_manager().call().await {
			Ok(result) => {
				let orc_manager_address = result;
				self.oracle_manager =
					Some(OracleManagerContract::new(orc_manager_address, self.client.provider()));
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] ✅ Oracle Manager resolved at {:?}",
					sub_display_format(SUB_LOG_TARGET),
					orc_manager_address
				);
			},
			Err(e) => {
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to resolve Oracle Manager address: {:?}",
					sub_display_format(SUB_LOG_TARGET),
					e
				);
			},
		}
	}

	/// Check price deviation and trigger feed if needed.
	async fn check_and_feed(&self) {
		let oracle_manager = match &self.oracle_manager {
			Some(om) => om,
			None => return,
		};

		// Fetch current market prices (VWAP)
		let mut market_prices = match self.fetch_vwap_prices(FetchMode::Standard).await {
			Ok(prices) => prices,
			Err(_) => {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚠️  Failed to fetch market prices for deviation check.",
					sub_display_format(SUB_LOG_TARGET),
				);
				return;
			},
		};

		// Fetch JPYC price from dedicated source, fallback to VWAP
		if let Some(ref source) = self.dedicated_source {
			match source.get_tickers().await {
				Ok(prices) => {
					market_prices.extend(prices);
				},
				Err(_) => {
					if let Ok(prices) =
						self.fetch_vwap_prices(FetchMode::Dedicated("JPYC".into())).await
					{
						market_prices.extend(prices);
					}
				},
			}
		}

		let mut deviated_oids: Vec<FixedBytes<32>> = vec![];
		let mut deviated_prices: Vec<FixedBytes<32>> = vec![];
		let mut deviated_responses: BTreeMap<String, PriceResponse> = BTreeMap::new();

		for (symbol, market_response) in &market_prices {
			let oid = match self.asset_oid.get(symbol.as_str()) {
				Some(oid) => *oid,
				None => continue,
			};

			if market_response.price.is_zero() {
				continue;
			}

			// Read on-chain oracle price
			let on_chain_price = match oracle_manager.last_oracle_data(oid).call().await {
				Ok(result) => U256::from_be_bytes(result.into()),
				Err(_) => continue,
			};

			if on_chain_price.is_zero() {
				continue;
			}

			// Calculate deviation: |market - oracle| * 10000 / oracle
			let deviation_bps = if market_response.price > on_chain_price {
				(market_response.price - on_chain_price) * U256::from(10_000) / on_chain_price
			} else {
				(on_chain_price - market_response.price) * U256::from(10_000) / on_chain_price
			};

			let threshold = U256::from(get_deviation_threshold_bps(symbol));

			if deviation_bps > threshold {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] 🚨 Price deviation detected for {}: \
					market={}, on-chain={}, deviation={}bps (threshold={}bps)",
					sub_display_format(SUB_LOG_TARGET),
					symbol,
					market_response.price,
					on_chain_price,
					deviation_bps,
					threshold
				);

				deviated_oids.push(oid);
				deviated_prices.push(market_response.price.into());
				deviated_responses.insert(symbol.clone(), market_response.clone());
			}
		}

		if !deviated_oids.is_empty() {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 📦 Triggering immediate feed for {} deviated assets",
				sub_display_format(SUB_LOG_TARGET),
				deviated_oids.len()
			);

			let tx_request = self
				.client
				.protocol_contracts
				.socket
				.oracle_aggregate_feeding(deviated_oids, deviated_prices)
				.into_transaction_request();

			send_transaction(
				self.client.clone(),
				tx_request,
				SUB_LOG_TARGET.to_string(),
				TxRequestMetadata::PriceFeed(PriceFeedMetadata::new(deviated_responses)),
				self.debug_mode,
				self.handle.clone(),
			);
		}
	}

	/// Fetch VWAP prices from market sources for the given mode.
	async fn fetch_vwap_prices(
		&self,
		mode: FetchMode,
	) -> Result<BTreeMap<String, PriceResponse>, std::fmt::Error> {
		let futures: Vec<_> = self
			.market_sources
			.iter()
			.filter(|f| f.mode() == mode)
			.map(|f| f.get_tickers())
			.collect();
		let results = futures::future::join_all(futures).await;

		let mut volume_weighted: BTreeMap<String, (U256, U256)> = BTreeMap::new();

		for result in results {
			if let Ok(tickers) = result {
				for (symbol, price_response) in &tickers {
					if let Some(volume) = price_response.volume {
						if !volume.is_zero() {
							if let Some(value) = volume_weighted.get_mut(symbol) {
								value.0 += price_response.price * volume;
								value.1 += volume;
							} else {
								volume_weighted.insert(
									symbol.clone(),
									(price_response.price * volume, volume),
								);
							}
						}
					}
				}
			}
		}

		if volume_weighted.is_empty() {
			return Err(std::fmt::Error);
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
}
