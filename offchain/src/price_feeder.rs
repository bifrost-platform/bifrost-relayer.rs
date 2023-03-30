use crate::price_source::PriceFetchers;
use async_trait::async_trait;
use cccp_primitives::{
	cli::PriceFeederConfig,
	offchain::{OffchainWorker, PriceFetcher, TimeDrivenOffchainWorker},
};
use ethers::{
	prelude::abigen,
	providers::{Http, Provider},
	types::{Eip1559TransactionRequest, H160},
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

pub struct OraclePriceFeeder {
	pub feed_interval: Duration,
	pub contract: SocketBifrost<Provider<Http>>,
	pub fetchers: Vec<PriceFetchers>,
	pub sender: Arc<UnboundedSender<Eip1559TransactionRequest>>,
	pub config: PriceFeederConfig,
	pub asset_oid: HashMap<String, [u8; 32]>,
}

#[async_trait]
impl OffchainWorker for OraclePriceFeeder {
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
impl TimeDrivenOffchainWorker for OraclePriceFeeder {
	async fn wait_until_next_time(&self) {
		let current_time = SystemTime::now();
		let duration = self
			.feed_interval
			.checked_sub(current_time.duration_since(SystemTime::UNIX_EPOCH).unwrap())
			.unwrap_or_else(|| Duration::from_secs(0));

		sleep(duration).await;
	}
}

impl OraclePriceFeeder {
	fn new(
		sender: Arc<UnboundedSender<Eip1559TransactionRequest>>,
		config: PriceFeederConfig,
		client: Arc<Provider<Http>>,
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
		let float_price = f64::from_str(value).unwrap();
		let wei_price = (float_price * 1_000_000_000_000_000_000f64) as u64;

		let mut bytes_price = [0u8; 32];
		bytes_price[24..32].copy_from_slice(&wei_price.to_be_bytes());

		bytes_price
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

// #[cfg(test)]
// mod tests {
// 	use super::*;
// 	use cccp_primitives::offchain::PriceSource;
// 	use tokio::sync::mpsc::{self};
//
// 	#[tokio::test]
// 	async fn wei_price_bytes_conversion() {
// 		todo!();
//
// 		let config = PriceFeederConfig {
// 			network_id: 49088,
// 			feed_interval: 300,
// 			contract: "0x252a402B80d081F672e7874E94214054FdB78C77".to_string(),
// 			price_sources: vec![PriceSource::Coingecko],
// 			symbols: vec!["BFC".to_string()],
// 		};
// 		let (sender, _) = mpsc::unbounded_channel::<Eip1559TransactionRequest>();
//
// 		let mut oracle_price_feeder = OraclePriceFeeder::new(Arc::new(sender), config);
// 		oracle_price_feeder.initialize_fetchers().await;
//
// 		let price_responses = oracle_price_feeder.fetchers[0].get_price().await;
//
// 		for price_response in price_responses {
// 			let bytes_price = oracle_price_feeder.float_to_wei_bytes(&price_response.price);
// 			assert_eq!(bytes_price.len(), 32, "Byte slice length must be 32 bytes");
// 			println!("String price: {:?}, Bytes price: {:?}", price_response.price, bytes_price);
// 		}
// 	}
// }
