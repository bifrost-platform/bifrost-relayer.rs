use std::sync::Arc;

use ethers::providers::JsonRpcClient;

use super::EthClient;

pub struct TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
}

impl<T: JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: Arc<EthClient<T>>) -> Self {
		Self { client }
	}

	pub async fn run(&self) {
		println!("a => {:?}", self.client.config.name);
		println!("a => {:?}", self.client.provider);
		// loop {
		// 	let now = Instant::now();

		// 	}
		// 	tokio::time::sleep_until(now + Duration::from_millis(3000)).await;
		// }
	}
}
