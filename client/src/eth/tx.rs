use ethers::{
	prelude::{NonceManagerMiddleware, SignerMiddleware},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::{LocalWallet, Signer},
	types::{TransactionRequest, U256},
};
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::{EthClient, TxResult};

/// The essential task that sends asynchronous transactions.
pub struct TransactionManager<T> {
	// /// The ethereum client for the connected chain.
	// pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	pub middleware: Arc<NonceManagerMiddleware<SignerMiddleware<Arc<Provider<T>>, LocalWallet>>>,
	/// The receiver connected to the event channel.
	pub receiver: UnboundedReceiver<TransactionRequest>,
}

impl<T: JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `TransactionManager` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, UnboundedSender<TransactionRequest>) {
		let (sender, receiver) = mpsc::unbounded_channel::<TransactionRequest>();

		let middleware = Arc::new(NonceManagerMiddleware::new(
			SignerMiddleware::new(client.provider.clone(), client.wallet.signer.clone()),
			client.wallet.signer.address(),
		));

		(Self { middleware, receiver }, sender)
	}

	/// Starts the transaction manager. Listens to every new consumed event message.
	pub async fn run(&mut self) {
		while let Some(msg) = self.receiver.recv().await {
			println!("msg -> {:?}", msg);

			self.send_transaction(msg).await.unwrap();
		}
	}

	async fn send_transaction(&mut self, msg: TransactionRequest) -> TxResult {
		let nonce = self.middleware.get_transaction_count(msg.from.unwrap(), None).await.unwrap();

		let (max_fee_per_gas, _max_priority_fee_per_gas) =
			self.middleware.estimate_eip1559_fees(None).await.unwrap();

		let tx = msg.gas(U256::from(1000000)).gas_price(max_fee_per_gas).nonce(nonce);

		let res = self
			.middleware
			.send_transaction(tx, None)
			.await
			.expect("fail to sending tx")
			.await
			.unwrap();

		println!("transaction result : {:?}", res);

		Ok(())
	}
}
