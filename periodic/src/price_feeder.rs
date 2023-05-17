use crate::price_source::PriceFetchers;
use async_trait::async_trait;
use cccp_client::eth::{EthClient, EventMessage, EventMetadata, EventSender, PriceFeedMetadata};
use cccp_primitives::{
	cli::PriceFeederConfig,
	errors::INVALID_PERIODIC_SCHEDULE,
	periodic::{PeriodicWorker, PriceFetcher},
	socket::get_asset_oids,
	sub_display_format, INVALID_BIFROST_NATIVENESS,
};
use cron::Schedule;
use ethers::{
	providers::JsonRpcClient,
	types::{TransactionRequest, H256, U256},
};
use std::{collections::HashMap, str::FromStr, sync::Arc};
use tokio::time::sleep;

const SUB_LOG_TARGET: &str = "price-oracle";

/// The essential task that handles oracle price feedings.
pub struct OraclePriceFeeder<T> {
	/// The time schedule that represents when to send price feeds.
	pub schedule: Schedule,
	/// The source for fetching prices.
	pub fetchers: Vec<PriceFetchers>,
	/// The event sender that sends messages to the event channel.
	pub event_sender: Arc<EventSender>,
	/// The price feeder configurations.
	pub config: PriceFeederConfig,
	/// The pre-defined oracle ID's for each asset.
	pub asset_oid: HashMap<String, H256>,
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
				let price_responses = self.fetchers[0].get_price().await;

				let (mut oid_bytes_list, mut price_bytes_list) = (vec![], vec![]);
				price_responses.iter().for_each(|price_response| {
					oid_bytes_list
						.push(self.asset_oid.get(&price_response.symbol).unwrap().to_fixed_bytes());
					price_bytes_list.push(self.float_to_wei_bytes(&price_response.price));
				});

				let request = self.build_transaction(oid_bytes_list, price_bytes_list).await;
				self.request_send_transaction(request, PriceFeedMetadata::new(price_responses))
					.await;
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
			fetchers: vec![],
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

	/// Initialize price fetchers. Can't move into new().
	async fn initialize_fetchers(&mut self) {
		for price_source in &self.config.price_sources {
			let fetcher =
				PriceFetchers::new(price_source.clone(), self.config.symbols.clone()).await;

			self.fetchers.push(fetcher);
		}
	}

	fn float_to_wei_bytes(&self, value: &str) -> [u8; 32] {
		U256::from((f64::from_str(value).unwrap() * 1_000_000_000_000_000_000f64) as u128).into()
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
			tx_request,
			EventMetadata::PriceFeed(metadata.clone()),
			false,
			false,
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
				sentry::capture_error(&error);
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
