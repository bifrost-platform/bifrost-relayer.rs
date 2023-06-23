use std::{collections::BTreeMap, fmt::Error, str::FromStr, sync::Arc};

use async_trait::async_trait;
use cron::Schedule;
use ethers::{
	providers::JsonRpcClient,
	types::{TransactionRequest, H256, U256},
	utils::parse_ether,
};
use tokio::time::sleep;

use cccp_client::eth::{
	EthClient, EventMessage, EventMetadata, EventSender, PriceFeedMetadata, TxRequest,
};
use cccp_primitives::{
	cli::PriceFeederConfig, errors::INVALID_PERIODIC_SCHEDULE, eth::GasCoefficient,
	periodic::PeriodicWorker, socket::get_asset_oids, sub_display_format, PriceFetcher,
	PriceResponse, PriceSource, INVALID_BIFROST_NATIVENESS,
};

use crate::price_source::PriceFetchers;

const SUB_LOG_TARGET: &str = "price-oracle";

/// The essential task that handles oracle price feedings.
pub struct OraclePriceFeeder<T> {
	/// The time schedule that represents when to send price feeds.
	pub schedule: Schedule,
	/// The primary source for fetching prices. (Coingecko)
	pub primary_source: Vec<PriceFetchers>,
	/// The secondary source for fetching prices. (aggregate from sources)
	pub secondary_sources: Vec<PriceFetchers>,
	/// The event sender that sends messages to the event channel.
	pub event_sender: Arc<EventSender>,
	/// The price feeder configurations.
	pub config: PriceFeederConfig,
	/// The pre-defined oracle ID's for each asset.
	pub asset_oid: BTreeMap<String, H256>,
	/// The `EthClient` to interact with the bifrost network.
	pub client: Arc<EthClient<T>>,
}

#[async_trait]
impl<T: JsonRpcClient> PeriodicWorker for OraclePriceFeeder<T> {
	async fn run(&mut self) {
		self.initialize_fetchers().await;

		loop {
			self.wait_until_next_time().await;

			if self.is_selected_relayer().await {
				if self.primary_source.len() == 0 {
					log::warn!(
						target: &self.client.get_chain_name(),
						"-[{}] ❗️ Failed to initialize primary fetcher. Try fetch with secondary sources.",
						sub_display_format(SUB_LOG_TARGET),
					);
					self.try_with_secondary().await;
				} else {
					self.try_with_primary().await;
				}
			}
		}
	}

	async fn wait_until_next_time(&self) {
		// calculate sleep duration for next schedule
		let sleep_duration =
			self.schedule.upcoming(chrono::Utc).next().unwrap() - chrono::Utc::now();

		sleep(sleep_duration.to_std().unwrap()).await;
	}
}

impl<T: JsonRpcClient> OraclePriceFeeder<T> {
	pub fn new(
		event_senders: Vec<Arc<EventSender>>,
		config: PriceFeederConfig,
		clients: Vec<Arc<EthClient<T>>>,
	) -> Self {
		let asset_oid = get_asset_oids();

		Self {
			schedule: Schedule::from_str(&config.schedule).expect(INVALID_PERIODIC_SCHEDULE),
			primary_source: vec![],
			secondary_sources: vec![],
			event_sender: event_senders
				.iter()
				.find(|event_sender| event_sender.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
			config,
			asset_oid,
			client: clients
				.iter()
				.find(|client| client.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
		}
	}

	async fn try_with_primary(&self) {
		match self.primary_source[0].get_tickers().await {
			// If coingecko works well.
			Ok(price_responses) => {
				self.build_and_send_transaction(price_responses).await;
			},
			// If coingecko works not well.
			Err(_) => {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to fetch price feed data from primary source. Retry fetch with secondary sources.",
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
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to fetch price feed data from secondary sources. First off, skip this feeding.",
					sub_display_format(SUB_LOG_TARGET),
				);
				sentry::capture_message(
					format!(
						"[{}] ❗️ Failed to fetch price feed data from secondary sources. First off, skip this feeding.",
						SUB_LOG_TARGET,
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	/// If price data fetch failed with primary source, try with secondary sources.
	async fn fetch_from_secondary(&self) -> Result<BTreeMap<String, PriceResponse>, Error> {
		// (volume weighted price sum, total volume)
		let mut volume_weighted: BTreeMap<String, (U256, U256)> = BTreeMap::new();

		for fetcher in self.secondary_sources.clone() {
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
			return Err(Error::default())
		}

		let mut res: BTreeMap<String, PriceResponse> = volume_weighted
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
			.collect();
		res.insert("USDT".into(), PriceResponse { price: parse_ether(1).unwrap(), volume: None });
		res.insert("USDC".into(), PriceResponse { price: parse_ether(1).unwrap(), volume: None });

		Ok(res)
	}

	/// Initialize price fetchers. Can't move into new().
	async fn initialize_fetchers(&mut self) {
		match PriceFetchers::new(PriceSource::Coingecko).await {
			Ok(primary) => {
				self.primary_source.push(primary);
			},
			Err(_) => {},
		}

		let secondary_sources = vec![
			PriceSource::Binance,
			PriceSource::Gateio,
			PriceSource::Kucoin,
			PriceSource::Upbit,
		];
		for source in secondary_sources {
			match PriceFetchers::new(source).await {
				Ok(fetcher) => {
					self.secondary_sources.push(fetcher);
				},
				Err(_) => continue,
			}
		}
	}

	/// Build and send transaction.
	async fn build_and_send_transaction(&self, price_responses: BTreeMap<String, PriceResponse>) {
		let mut oid_bytes_list: Vec<[u8; 32]> = vec![];
		let mut price_bytes_list: Vec<[u8; 32]> = vec![];

		price_responses.iter().for_each(|(symbol, price_response)| {
			oid_bytes_list.push(self.asset_oid.get(symbol).unwrap().to_fixed_bytes());
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
		TransactionRequest::default().to(self.client.socket.address()).data(
			self.client
				.socket
				.oracle_aggregate_feeding(oid_bytes_list, price_bytes_list)
				.calldata()
				.unwrap(),
		)
	}

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: PriceFeedMetadata,
	) {
		match self.event_sender.send(EventMessage::new(
			TxRequest::Legacy(tx_request),
			EventMetadata::PriceFeed(metadata.clone()),
			false,
			false,
			GasCoefficient::Mid,
		)) {
			Ok(()) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 💵 Request price feed transaction to chain({:?}): {}",
				sub_display_format(SUB_LOG_TARGET),
				self.config.chain_id,
				metadata
			),
			Err(error) => {
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to request price feed transaction to chain({:?}): {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.config.chain_id,
					metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to request price feed transaction to chain({:?}): {}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						self.config.chain_id,
						metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	/// Verifies whether the current relayer was selected at the current round.
	async fn is_selected_relayer(&self) -> bool {
		let relayer_manager = self.client.relayer_manager.as_ref().unwrap();
		self.client
			.contract_call(
				relayer_manager.is_selected_relayer(self.client.address(), false),
				"relayer_manager.is_selected_relayer",
			)
			.await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn secondary_fetch() {
		let mut a = vec![];
		a.push(PriceFetchers::new(PriceSource::Binance).await.unwrap());
		a.push(PriceFetchers::new(PriceSource::Gateio).await.unwrap());
		a.push(PriceFetchers::new(PriceSource::Kucoin).await.unwrap());
		a.push(PriceFetchers::new(PriceSource::Upbit).await.unwrap());

		let res: Result<BTreeMap<String, PriceResponse>, Error> = {
			// (volume weighted price sum, total volume)
			let mut volume_weighted: BTreeMap<String, (U256, U256)> = BTreeMap::new();

			for fetcher in a.clone() {
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
		};

		println!("{:#?}", res);
	}
}
