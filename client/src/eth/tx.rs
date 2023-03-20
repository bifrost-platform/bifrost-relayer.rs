use std::sync::Arc;

use ethers::providers::JsonRpcClient;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::eth::{PollSubmit, Signatures};

use super::{EthClient, SocketMessage};

/// The essential task that sends relay transactions.
pub struct TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The channel receiving socket messages.
	pub receiver: Receiver<SocketMessage>,
}

impl<T: JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, Sender<SocketMessage>) {
		let (sender, receiver) = mpsc::channel::<SocketMessage>(32);
		(Self { client, receiver }, sender)
	}

	/// Starts the transaction manager. Listens to every new consumed socket message.
	pub async fn run(&mut self) {
		while let Some(msg) = self.receiver.recv().await {
			println!("msg -> {:?}", msg);

			let poll_submit = PollSubmit {
				msg,
				sigs: Signatures::default(),
				option: ethers::types::U256::default(),
			};

			match self.client.socket.poll(poll_submit).call().await {
				Ok(result) => {
					println!("result : {result:?}")
				},
				Err(e) => {
					println!("error : {e:?}")
				},
			}
		}
	}
}
