use ethers::{
	prelude::{NonceManagerMiddleware, SignerMiddleware},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::{LocalWallet, Signer},
	types::{Eip1559TransactionRequest, U256},
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
	pub receiver: UnboundedReceiver<Eip1559TransactionRequest>,
}

impl<T: 'static + JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `TransactionManager` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, UnboundedSender<Eip1559TransactionRequest>) {
		let (sender, receiver) = mpsc::unbounded_channel::<Eip1559TransactionRequest>();

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

	async fn send_transaction(&mut self, msg: Eip1559TransactionRequest) -> TxResult {
		let nonce = self.middleware.get_transaction_count(msg.from.unwrap(), None).await.unwrap();

		let (max_fee_per_gas, max_priority_fee_per_gas) =
			self.middleware.estimate_eip1559_fees(None).await?;

		let tx = msg
			.gas(U256::from(1000000))
			.max_fee_per_gas(max_fee_per_gas)
			.max_priority_fee_per_gas(max_priority_fee_per_gas)
			.nonce(nonce);

		let pending_tx = self.middleware.send_transaction(tx.clone(), None).await?;

		let receipt = pending_tx.await?.ok_or("tx dropped".to_string())?;

		println!("transaction result : {:?}", receipt);

		match self.middleware.get_transaction(receipt.transaction_hash).await? {
			Some(t) => println!("Sent tx: {:?}", t),
			None => {
				println!("retry to send tx");
				self.middleware.send_transaction(tx, None).await?.await?;
			},
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use ethers::{
		prelude::MiddlewareBuilder,
		providers::{Http, Middleware, Provider},
		signers::{LocalWallet, Signer},
		types::{BlockNumber, TransactionRequest},
		utils::{Anvil, AnvilInstance},
	};

	pub fn spawn_anvil() -> (Provider<Http>, AnvilInstance) {
		let anvil = Anvil::new().block_time(1u64).chain_id(1337_u64).spawn();
		let provider = Provider::<Http>::try_from(anvil.endpoint())
			.unwrap()
			.interval(Duration::from_millis(50u64));
		(provider, anvil)
	}

	#[tokio::test]
	async fn nonce_manager() {
		let (provider, anvil) = spawn_anvil();
		let address = anvil.addresses()[0];
		let to = anvil.addresses()[1];

		let provider = provider.nonce_manager(address);

		let nonce = provider
			.get_transaction_count(address, Some(BlockNumber::Pending.into()))
			.await
			.unwrap()
			.as_u64();

		println!("nonce : {:?}", nonce);

		let num_tx = 3;
		let mut tx_hashes = Vec::with_capacity(num_tx);
		for _ in 0..num_tx {
			let tx = provider
				.send_transaction(
					TransactionRequest::new().from(address).to(to).value(100u64),
					None,
				)
				.await
				.unwrap();
			tx_hashes.push(*tx);
		}

		println!("tx_hashes : {:?}", &tx_hashes);

		let mut nonces = Vec::with_capacity(num_tx);
		for tx_hash in tx_hashes {
			nonces.push(provider.get_transaction(tx_hash).await.unwrap().unwrap().nonce.as_u64());
		}

		println!("nonces : {:?}", nonces);

		assert_eq!(nonces, (nonce..nonce + num_tx as u64).collect::<Vec<_>>());
	}

	#[tokio::test]
	async fn send_transaction() {
		let (provider, anvil) = spawn_anvil();
		let wallet1: LocalWallet = anvil.keys()[0].clone().into();
		let address = wallet1.address();

		let wallet2: LocalWallet = anvil.keys()[1].clone().into();
		let to = wallet2.address();

		let provider = provider;
		// let provider = provider.with_signer(wallet1).nonce_manager(address);

		let nonce = provider
			.get_transaction_count(address, Some(BlockNumber::Pending.into()))
			.await
			.unwrap()
			.as_u64();

		let num_tx = 3;
		let mut tx_hashes = Vec::with_capacity(num_tx);
		for _ in 0..num_tx {
			let nonce = provider.get_transaction_count(address, None).await.unwrap();
			println!("nonce is increased? : {:?}", nonce);

			let tx = provider
				.send_transaction(
					TransactionRequest::new().from(address).to(to).value(100u64),
					None,
				)
				.await
				.unwrap()
				.await
				.unwrap();

			tx_hashes.push(tx.unwrap());
		}
		println!("tx : {:?}", &tx_hashes);

		let mut nonces = Vec::with_capacity(num_tx);
		for tx_hash in tx_hashes {
			nonces.push(
				provider
					.get_transaction(tx_hash.transaction_hash)
					.await
					.unwrap()
					.unwrap()
					.nonce
					.as_u64(),
			);
		}

		println!("nonces : {:?}", &nonces);

		assert_eq!(nonces, (nonce..nonce + num_tx as u64).collect::<Vec<_>>());
	}
}
