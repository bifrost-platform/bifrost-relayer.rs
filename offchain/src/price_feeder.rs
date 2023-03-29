use crate::price_source::PriceFetchers;
use async_trait::async_trait;
use cccp_primitives::{
	cli::PriceFeederConfig,
	offchain::{OffchainWorker, TimeDrivenOffchainWorker},
};
use ethers::{
	prelude::{abigen, TransactionRequest},
	types::H160,
};
use std::{str::FromStr, sync::Arc, time::SystemTime};
use tokio::{
	sync::mpsc::UnboundedSender,
	time::{sleep, Duration},
};

abigen!(
	Oracle,
	"../abi/abi.oracle.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

pub struct OraclePriceFeeder {
	pub feed_interval: Duration,
	pub contract: H160,
	pub fetchers: Vec<PriceFetchers>,
	pub sender: Arc<UnboundedSender<TransactionRequest>>,
	pub config: PriceFeederConfig,
}

#[async_trait]
impl OffchainWorker for OraclePriceFeeder {
	async fn run(&mut self) {
		self.initialize_fetchers().await;

		loop {
			self.wait_until_next_time().await;

			// TODO: fetch prices and make price feed data

			let request = self.build_transaction().await;
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
		feed_interval: u64,
		contract: String,
		sender: Arc<UnboundedSender<TransactionRequest>>,
		config: PriceFeederConfig,
	) -> Self {
		Self {
			feed_interval: Duration::from_secs(feed_interval),
			contract: H160::from_str(&contract).unwrap(),
			fetchers: vec![],
			sender,
			config,
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

	/// Build transaction.
	async fn build_transaction(&self) -> TransactionRequest {
		todo!()
	}

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(&self, request: TransactionRequest) {
		match self.sender.send(request) {
			Ok(()) => println!("Oracle price feed request sent successfully"),
			Err(e) => println!("Failed to send oracle price feed request: {}", e),
		}
	}
}
