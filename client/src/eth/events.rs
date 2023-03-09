use web3::{types::U64, Transport};

use super::EthClient;

#[derive(Debug, Clone)]
pub struct EventDetector<T: Transport> {
	pub client: EthClient<T>,
	pub queue: Vec<U64>,
}

impl<T: Transport> EventDetector<T> {
	pub fn new(client: EthClient<T>) -> Self {
		Self { client, queue: vec![] }
	}

	pub async fn run(&self) {
		loop {
			let block_number = self.client.get_block_number().await.unwrap();
			println!("block number: {:?}", block_number);
		}
	}
}
