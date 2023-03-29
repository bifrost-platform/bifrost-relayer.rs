use ethers::{
	prelude::{
		gas_escalator::{Frequency, GasEscalatorMiddleware, GeometricGasPrice},
		NonceManagerMiddleware, SignerMiddleware,
	},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::{LocalWallet, Signer},
	types::transaction::eip2718::TypedTransaction,
};
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::{EthClient, EventMessage};

/// The essential task that sends asynchronous transactions.
pub struct TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	pub middleware: NonceManagerMiddleware<
		SignerMiddleware<GasEscalatorMiddleware<Arc<Provider<T>>, GeometricGasPrice>, LocalWallet>,
	>,
	/// The sender connected to the event channel.
	pub sender: UnboundedSender<EventMessage>,
	/// The receiver connected to the event channel.
	pub receiver: UnboundedReceiver<EventMessage>,
}

impl<T: 'static + JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `TransactionManager` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, UnboundedSender<EventMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<EventMessage>();

		// Bumps transactions gas price in the background to avoid getting them stuck in the memory
		// pool.
		let geometric_gas_price = GeometricGasPrice::new(1.125, 60u64, None::<u64>);
		let gas_escalator = GasEscalatorMiddleware::new(
			client.provider.clone(),
			geometric_gas_price,
			Frequency::PerBlock,
		);
		// Signs transactions locally, with a private key or a hardware wallet.
		let signer = SignerMiddleware::new(gas_escalator, client.wallet.signer.clone());
		// Manages nonces locally. Allows to sign multiple consecutive transactions without waiting
		// for them to hit the mempool.
		let middleware = NonceManagerMiddleware::new(signer, client.wallet.signer.address());

		(Self { client, sender: sender.clone(), middleware, receiver }, sender)
	}

	/// Starts the transaction manager. Listens to every new consumed event message.
	pub async fn run(&mut self) {
		while let Some(msg) = self.receiver.recv().await {
			println!("msg -> {:?}", msg);

			self.send_transaction(msg).await;
		}
	}

	async fn send_transaction(&self, msg: EventMessage) {
		if msg.retries_remaining == 0 {
			println!("Exceeded the retry limit for sending a transaction");
			// TODO: retry 전부 소모시 정책 결정 필요
		}

		// get the next nonce to be used
		let nonce = self.middleware.next();

		let (max_fee_per_gas, max_priority_fee_per_gas) =
			self.middleware.estimate_eip1559_fees(None).await.unwrap();

		let estimated_gas = self
			.middleware
			.estimate_gas(&TypedTransaction::Eip1559(msg.tx_request.clone()), None)
			.await
			.unwrap();

		let final_tx = msg
			.tx_request
			.clone()
			.gas(estimated_gas)
			.max_fee_per_gas(max_fee_per_gas)
			.max_priority_fee_per_gas(max_priority_fee_per_gas)
			.nonce(nonce);

		match self.middleware.send_transaction(final_tx.clone(), None).await {
			Ok(pending_tx) => match pending_tx.await {
				Ok(receipt) =>
					if let Some(receipt) = receipt {
						println!(
							"The requested transaction has successfully mined in a block: {:?}",
							receipt.transaction_hash
						);
					} else {
						println!("The requested transaction has been dropped from the mempool");
						self.sender
							.send(EventMessage::new(msg.retries_remaining - 1, msg.tx_request))
							.unwrap();
					},
				Err(error) => {
					println!("Unknown error while waiting for transaction receipt: {:?}", error);
					self.sender
						.send(EventMessage::new(msg.retries_remaining - 1, msg.tx_request))
						.unwrap();
				},
			},
			Err(error) => {
				println!("Unknown error while sending transaction: {:?}", error);
				self.sender
					.send(EventMessage::new(msg.retries_remaining - 1, msg.tx_request))
					.unwrap();
			},
		}
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
