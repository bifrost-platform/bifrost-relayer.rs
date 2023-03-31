use crate::price_source::PriceFetchers;
use async_trait::async_trait;
use cccp_primitives::{
	cli::PriceFeederConfig,
	offchain::{OffchainWorker, PriceFetcher, TimeDrivenOffchainWorker},
};
use ethers::{
	prelude::abigen,
	providers::{JsonRpcClient, Provider},
	types::{Eip1559TransactionRequest, H160, U256},
	utils::hex,
};
use std::{collections::HashMap, str::FromStr, sync::Arc, time::SystemTime};
use tokio::{
	sync::mpsc::UnboundedSender,
	time::{sleep, Duration},
};

abigen!(
	SocketBifrost,
	"../abi/abi.socket.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

pub struct OraclePriceFeeder<T> {
	pub feed_interval: Duration,
	pub contract: SocketBifrost<Provider<T>>,
	pub fetchers: Vec<PriceFetchers>,
	pub sender: Arc<UnboundedSender<Eip1559TransactionRequest>>,
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
			for price_response in price_responses {
				oid_bytes_list.push(self.asset_oid.get(&price_response.symbol).unwrap().clone());
				price_bytes_list.push(self.float_to_wei_bytes(&price_response.price));
			}

			let request = self.build_transaction(oid_bytes_list, price_bytes_list).await;
			self.request_send_transaction(request).await;
		}
	}
}

#[async_trait]
impl<T: JsonRpcClient> TimeDrivenOffchainWorker for OraclePriceFeeder<T> {
	async fn wait_until_next_time(&self) {
		let current_time = SystemTime::now();
		let duration = self
			.feed_interval
			.checked_sub(current_time.duration_since(SystemTime::UNIX_EPOCH).unwrap())
			.unwrap_or_else(|| Duration::from_secs(0));

		sleep(duration).await;
	}
}

impl<T: JsonRpcClient> OraclePriceFeeder<T> {
	fn new(
		sender: Arc<UnboundedSender<Eip1559TransactionRequest>>,
		config: PriceFeederConfig,
		client: Arc<Provider<T>>,
	) -> Self {
		let asset_oid: HashMap<String, [u8; 32]> = HashMap::from([
			(
				"BFC".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000001")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"BIFI".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000002")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"BTC".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000003")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"ETH".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000004")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"BNB".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000005")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"MATIC".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000006")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"AVAX".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000007")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"USDC".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000008")
					.unwrap()
					.try_into()
					.unwrap(),
			),
			(
				"BUSD".to_string(),
				hex::decode("0100010000000000000000000000000000000000000000000000000000000009")
					.unwrap()
					.try_into()
					.unwrap(),
			),
		]);

		Self {
			feed_interval: Duration::from_secs(config.feed_interval),
			contract: SocketBifrost::new(H160::from_str(&config.contract).unwrap(), client.clone()),
			fetchers: vec![],
			sender,
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
	) -> Eip1559TransactionRequest {
		Eip1559TransactionRequest::new().to(self.contract.address()).data(
			self.contract
				.oracle_aggregate_feeding(oid_bytes_list, price_bytes_list)
				.calldata()
				.unwrap(),
		)
	}

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(&self, request: Eip1559TransactionRequest) {
		match self.sender.send(request) {
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

	#[tokio::test]
	async fn build_price_feeding_transaction() {
		let config_file =
			std::fs::File::open("../config.yaml").expect("Could not open config file.");
		let relayer_config: RelayerConfig =
			serde_yaml::from_reader(config_file).expect("Config file not valid.");
		let evm_provider = relayer_config
			.evm_providers
			.into_iter()
			.find(|evm_provider| evm_provider.name == "bfc-testnet")
			.unwrap();
		let (sender, _) = mpsc::unbounded_channel::<Eip1559TransactionRequest>();
		let mut oracle_price_feeder = OraclePriceFeeder::new(
			Arc::new(sender),
			relayer_config.oracle_price_feeder.unwrap(),
			Arc::new(Provider::<Http>::try_from(evm_provider.provider).unwrap()),
		);
		println!("oid_bytes: {:?}", oracle_price_feeder.asset_oid);

		oracle_price_feeder.initialize_fetchers().await;

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
}
