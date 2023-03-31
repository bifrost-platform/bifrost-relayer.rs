use crate::price_source::PriceFetchers;
use async_trait::async_trait;
use cccp_client::eth::{EventMessage, EventSender};
use cccp_primitives::{
	cli::PriceFeederConfig,
	offchain::{get_asset_oids, OffchainWorker, PriceFetcher, TimeDrivenOffchainWorker},
};
use cron::Schedule;
use ethers::{
	prelude::abigen,
	providers::{JsonRpcClient, Provider},
	types::{TransactionRequest, H160, U256},
};
use std::{collections::HashMap, str::FromStr, sync::Arc};
use tokio::time::sleep;

abigen!(
	SocketBifrost,
	"../abi/abi.socket.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

pub struct OraclePriceFeeder<T> {
	pub schedule: Schedule,
	pub contract: SocketBifrost<Provider<T>>,
	pub fetchers: Vec<PriceFetchers>,
	pub event_sender: Arc<EventSender>,
	pub config: PriceFeederConfig,
	pub asset_oid: HashMap<String, [u8; 32]>,
}

#[async_trait]
impl<T: JsonRpcClient> OffchainWorker for OraclePriceFeeder<T> {
	async fn run(&mut self) {
		self.initialize_fetchers().await;

		loop {
			self.wait_until_next_time().await;

			let price_responses = self.fetchers[0].get_price().await;

			let (mut oid_bytes_list, mut price_bytes_list) = (vec![], vec![]);
			price_responses.iter().for_each(|price_response| {
				oid_bytes_list.push(self.asset_oid.get(&price_response.symbol).unwrap().clone());
				price_bytes_list.push(self.float_to_wei_bytes(&price_response.price));
			});

			let request = self.build_transaction(oid_bytes_list, price_bytes_list).await;
			self.request_send_transaction(request).await;
		}
	}
}

#[async_trait]
impl<T: JsonRpcClient> TimeDrivenOffchainWorker for OraclePriceFeeder<T> {
	async fn wait_until_next_time(&self) {
		// calculate sleep duration for next schedule
		let sleep_duration =
			self.schedule.upcoming(chrono::Utc).next().unwrap() - chrono::Utc::now();

		sleep(sleep_duration.to_std().unwrap()).await;
	}
}

impl<T: JsonRpcClient> OraclePriceFeeder<T> {
	pub fn new(
		event_sender: Arc<EventSender>,
		config: PriceFeederConfig,
		client: Arc<Provider<T>>,
	) -> Self {
		let asset_oid = get_asset_oids();

		Self {
			schedule: Schedule::from_str(&config.schedule).unwrap(),
			contract: SocketBifrost::new(H160::from_str(&config.contract).unwrap(), client.clone()),
			fetchers: vec![],
			event_sender,
			config,
			asset_oid,
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

	/// Build transaction.
	async fn build_transaction(
		&self,
		oid_bytes_list: Vec<[u8; 32]>,
		price_bytes_list: Vec<[u8; 32]>,
	) -> TransactionRequest {
		TransactionRequest::new().to(self.contract.address()).data(
			self.contract
				.oracle_aggregate_feeding(oid_bytes_list, price_bytes_list)
				.calldata()
				.unwrap(),
		)
	}

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(&self, request: TransactionRequest) {
		match self
			.event_sender
			.sender
			.send(EventMessage { retries_remaining: 10, tx_request: request })
		{
			Ok(()) => println!("Oracle price feed request sent successfully"),
			Err(e) => println!("Failed to send oracle price feed request: {}", e),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cccp_primitives::cli::RelayerConfig;
	use ethers::providers::Http;
	use tokio::sync::mpsc::{self};

	async fn initialize_feeder() -> OraclePriceFeeder<Http> {
		let config_file =
			std::fs::File::open("../config.yaml").expect("Could not open config file.");
		let relayer_config: RelayerConfig =
			serde_yaml::from_reader(config_file).expect("Config file not valid.");
		let evm_provider = relayer_config
			.evm_providers
			.into_iter()
			.find(|evm_provider| evm_provider.name == "bfc-testnet")
			.unwrap();
		let (sender, _) = mpsc::unbounded_channel::<EventMessage>();
		let event_sender = EventSender { id: evm_provider.id, sender };

		let mut oracle_price_feeder = OraclePriceFeeder::new(
			Arc::new(event_sender),
			relayer_config.offchain_configs.unwrap().oracle_price_feeder.unwrap()[0].clone(),
			Arc::new(Provider::<Http>::try_from(evm_provider.provider).unwrap()),
		);
		oracle_price_feeder.initialize_fetchers().await;

		oracle_price_feeder
	}

	#[tokio::test]
	async fn build_price_feeding_transaction() {
		let oracle_price_feeder = initialize_feeder().await;
		println!("oid_bytes: {:?}", oracle_price_feeder.asset_oid);

		let price_responses = oracle_price_feeder.fetchers[0].get_price().await;
		println!("price_responses: {:#?}", price_responses);

		let (mut oid_bytes_list, mut price_bytes_list) = (vec![], vec![]);
		for price_response in price_responses {
			oid_bytes_list
				.push(oracle_price_feeder.asset_oid.get(&price_response.symbol).unwrap().clone());
			price_bytes_list.push(oracle_price_feeder.float_to_wei_bytes(&price_response.price));
		}
		let request = oracle_price_feeder
			.build_transaction(oid_bytes_list.clone(), price_bytes_list.clone())
			.await;
		println!("oid_bytes_list: {:?}", oid_bytes_list);
		println!("price_bytes_list: {:?}", price_bytes_list);

		println!("price relay transaction: {:#?}", request);
	}

	#[tokio::test]
	async fn test_scheduler() {
		let oracle_price_feeder = initialize_feeder().await;

		println!("{:#?}", chrono::Utc::now());
		oracle_price_feeder.wait_until_next_time().await;
		println!("{:#?}", chrono::Utc::now());
	}
}
