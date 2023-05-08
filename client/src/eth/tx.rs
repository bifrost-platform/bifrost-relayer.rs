use async_recursion::async_recursion;
use cccp_primitives::sub_display_format;
use ethers::{
	prelude::{
		gas_escalator::{Frequency, GasEscalatorMiddleware, GeometricGasPrice},
		NonceManagerMiddleware, SignerMiddleware,
	},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::{LocalWallet, Signer},
	types::{transaction::eip2718::TypedTransaction, TransactionReceipt, TransactionRequest, U256},
};
use std::{error::Error, sync::Arc};
use tokio::{
	sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
	time::{sleep, Duration},
};

use super::{EthClient, EventMessage, EventMetadata, GAS_COEFFICIENT};

type TransactionMiddleware<T> =
	NonceManagerMiddleware<SignerMiddleware<GasEscalatorMiddleware<Arc<Provider<T>>>, LocalWallet>>;

const SUB_LOG_TARGET: &str = "transaction-manager";

/// The essential task that sends asynchronous transactions.
pub struct TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	pub middleware: TransactionMiddleware<T>,
	/// The sender connected to the event channel.
	pub sender: UnboundedSender<EventMessage>,
	/// The receiver connected to the event channel.
	pub receiver: UnboundedReceiver<EventMessage>,
	/// The flag whether the client has enabled txpool namespace.
	pub is_txpool_enabled: bool,
}

impl<T: 'static + JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `TransactionManager` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, UnboundedSender<EventMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<EventMessage>();

		// Bumps transactions gas price in the background to avoid getting them stuck in the memory
		// pool.
		let geometric_gas_price = GeometricGasPrice::new(1.125, 12u64, None::<u64>);
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

		(
			Self { client, sender: sender.clone(), middleware, receiver, is_txpool_enabled: false },
			sender,
		)
	}

	/// Initialize transaction manager.
	async fn initialize(&mut self) {
		let is_txpool_enabled = self.client.provider.txpool_content().await.is_ok();
		self.set_txpool_activation(is_txpool_enabled);
	}

	/// Starts the transaction manager. Listens to every new consumed event message.
	pub async fn run(&mut self) {
		self.initialize().await;

		while let Some(msg) = self.receiver.recv().await {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔖 Received transaction request: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata,
			);

			self.try_send_transaction(msg).await;
		}
	}

	/// Sends the transaction request to the event channel to retry transaction execution.
	#[async_recursion]
	async fn retry_transaction(&self, mut msg: EventMessage) {
		msg.build_retry_event();
		sleep(Duration::from_millis(msg.retry_interval)).await;
		self.try_send_transaction(msg).await;
	}

	/// Set the activation of txpool namespace related actions.
	fn set_txpool_activation(&mut self, is_txpool_enabled: bool) {
		self.is_txpool_enabled = is_txpool_enabled;
	}

	/// Handles the successfully mined transaction.
	fn handle_success_tx_receipt(&self, receipt: TransactionReceipt, metadata: EventMetadata) {
		let status = receipt.status.unwrap();
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 🎁 The requested transaction has been successfully mined in block: {}, {:?}-{:?}-{:?}",
			sub_display_format(SUB_LOG_TARGET),
			metadata.to_string(),
			receipt.block_number.unwrap(),
			status,
			receipt.transaction_hash
		);
		if status.is_zero() {
			log::warn!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚠️  Warning! Error encountered during contract execution [execution reverted]. A prior transaction might have been already submitted: {}, {:?}-{:?}-{:?}",
				sub_display_format(SUB_LOG_TARGET),
				metadata,
				receipt.block_number.unwrap(),
				status,
				receipt.transaction_hash
			);
			sentry::capture_message(
				format!(
					"[{}]-[{}] ⚠️  Warning! Error encountered during contract execution [execution reverted]. A prior transaction might have been already submitted: {}, {:?}-{:?}-{:?}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					metadata,
					receipt.block_number.unwrap(),
					status,
					receipt.transaction_hash
				)
				.as_str(),
				sentry::Level::Warning,
			);
		}
	}

	/// Handles the failed transaction receipt generation.
	async fn handle_failed_tx_receipt(&self, msg: EventMessage) {
		log::warn!(
			target: &self.client.get_chain_name(),
			"-[{}] ♻️  The requested transaction failed to generate a receipt: {}, Retries left: {:?}",
			sub_display_format(SUB_LOG_TARGET),
			msg.metadata,
			msg.retries_remaining - 1,
		);
		sentry::capture_message(
			format!(
				"[{}]-[{}] ♻️  The requested transaction failed to generate a receipt: {}, Retries left: {:?}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				msg.metadata,
				msg.retries_remaining - 1,
			)
			.as_str(),
			sentry::Level::Warning,
		);
		self.retry_transaction(msg).await;
	}

	/// Handles the failed transaction request.
	async fn handle_failed_tx_request<E>(&self, msg: EventMessage, error: &E)
	where
		E: Error + ?Sized,
	{
		log::error!(
			target: &self.client.get_chain_name(),
			"-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(SUB_LOG_TARGET),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		sentry::capture_error(&error);
		self.retry_transaction(msg).await;
	}

	/// Handles the failed gas estimation.
	async fn handle_failed_gas_estimation<E>(&self, msg: EventMessage, error: &E)
	where
		E: Error + ?Sized,
	{
		log::debug!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚠️  Warning! Error encountered during gas estimation: {}, Retries left: {:?}, Error: {}",
			sub_display_format(SUB_LOG_TARGET),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string()
		);
		self.retry_transaction(msg).await;
	}

	/// Sends the consumed transaction request to the connected chain. The transaction request will
	/// be re-published to the event channel if the transaction fails to be mined in a block.
	async fn try_send_transaction(&self, mut msg: EventMessage) {
		if msg.retries_remaining == 0 {
			log::warn!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚠️  Warning! Exceeded the transaction retry limit. A prior transaction might have been already submitted: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata
			);
			sentry::capture_message(
				format!(
					"[{}]-[{}] ⚠️  Warning! Exceeded the transaction retry limit. A prior transaction might have been already submitted: {}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					msg.metadata,
				)
				.as_str(),
				sentry::Level::Warning,
			);
			return
		}

		// set transaction `from` field
		msg.tx_request = msg.tx_request.from(self.client.address());

		// estimate the gas amount to be used
		let estimated_gas = match self
			.middleware
			.estimate_gas(&TypedTransaction::Legacy(msg.tx_request.clone()), None)
			.await
		{
			Ok(estimated_gas) => estimated_gas,
			Err(error) => return self.handle_failed_gas_estimation(msg, &error).await,
		};
		let escalated_gas =
			U256::from((estimated_gas.as_u64() as f64 * GAS_COEFFICIENT).ceil() as u64);
		msg.tx_request = msg.tx_request.gas(escalated_gas);

		// set the gas price to be used
		let gas_price = self.middleware.get_gas_price().await.unwrap();
		msg.tx_request = msg.tx_request.gas_price(gas_price);

		// check the txpool for transaction duplication prevention
		if !(self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await) {
			// no duplication found
			match self.middleware.send_transaction(msg.tx_request.clone(), None).await {
				Ok(pending_tx) => match pending_tx.await {
					Ok(receipt) =>
						if let Some(receipt) = receipt {
							self.handle_success_tx_receipt(receipt, msg.metadata);
						} else {
							self.handle_failed_tx_receipt(msg).await;
						},
					Err(error) => {
						self.handle_failed_tx_request(msg, &error).await;
					},
				},
				Err(error) => {
					self.handle_failed_tx_request(msg, &error).await;
				},
			};
		}
	}

	/// Function that query mempool for check if the relay event that this relayer is about to send
	/// has already been processed by another relayer.
	async fn is_duplicate_relay(
		&self,
		tx_request: &TransactionRequest,
		check_mempool: bool,
	) -> bool {
		// does not check the txpool if the following condition satisfies
		// 1. the txpool namespace is disabled for the client
		// 2. the txpool check flag is false
		// 3. the client is BIFROST (native)
		if !self.is_txpool_enabled || !check_mempool || self.client.is_native {
			return false
		}

		let data = tx_request.data.as_ref().unwrap();
		let to = tx_request.to.as_ref().unwrap().as_address().unwrap();

		let mempool_pending_contents = self.client.provider.txpool_content().await.unwrap().pending;
		for (_address, tx_map) in mempool_pending_contents.iter() {
			for (_nonce, transaction) in tx_map.iter() {
				if transaction.to.unwrap_or_default() == *to && transaction.input == *data {
					// Trying gas escalating is not duplicate action
					if transaction.from == tx_request.from.unwrap() &&
						transaction.nonce == tx_request.nonce.unwrap()
					{
						return false
					}
					return true
				}
			}
		}
		false
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
