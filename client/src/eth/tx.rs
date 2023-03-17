use std::sync::Arc;

use ethers::providers::JsonRpcClient;
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::{EthClient, SocketMessage};

pub struct TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	pub receiver: Receiver<SocketMessage>,
}

impl<T: JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, Sender<SocketMessage>) {
		let (sender, receiver) = mpsc::channel::<SocketMessage>(32);
		(Self { client, receiver }, sender)
	}

	pub async fn run(&mut self) {
		while let Some(msg) = self.receiver.recv().await {
			println!("msg -> {:?}", msg);
		}
	}
}
