use async_recursion::async_recursion;
use cccp_primitives::sub_display_format;

use crate::eth::{FlushMetadata, DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS};
use ethers::{
	prelude::{
		gas_escalator::{Frequency, GasEscalatorMiddleware, GeometricGasPrice},
		NonceManagerMiddleware, SignerMiddleware,
	},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::{LocalWallet, Signer},
	types::{
		transaction::eip2718::TypedTransaction, Transaction, TransactionReceipt,
		TransactionRequest, U256,
	},
};
use rand::Rng;
use std::{cmp::max, error::Error, sync::Arc};
use tokio::{
	sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
	time::{sleep, Duration},
};

use super::{EthClient, EventMessage, EventMetadata, DEFAULT_TX_RETRIES, GAS_COEFFICIENT};

type TransactionMiddleware<T> =
	NonceManagerMiddleware<SignerMiddleware<GasEscalatorMiddleware<Arc<Provider<T>>>, LocalWallet>>;

const SUB_LOG_TARGET: &str = "transaction-manager";

/// Generates a random delay that is ranged as 0 to 12 seconds (in milliseconds).
fn generate_delay() -> u64 {
	let delay: u64 = rand::thread_rng().gen_range(0..=12);
	delay.saturating_mul(1_000)
}

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
	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged.
	pub debug_mode: bool,
}

impl<T: 'static + JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `TransactionManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		debug_mode: bool,
	) -> (Self, UnboundedSender<EventMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<EventMessage>();

		// Bumps transactions gas price in the background to avoid getting them stuck in the memory
		// pool.
		let geometric_gas_price = GeometricGasPrice::new(1.5, 12u64, None::<u64>);
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
			Self {
				client,
				sender: sender.clone(),
				middleware,
				receiver,
				is_txpool_enabled: false,
				debug_mode,
			},
			sender,
		)
	}

	/// Initialize transaction manager.
	async fn initialize(&mut self) {
		self.is_txpool_enabled = self.client.provider.txpool_status().await.is_ok();
		self.flush_stuck_transaction().await;
	}

	/// Flush all transaction from mempool.
	async fn flush_stuck_transaction(&self) {
		if self.is_txpool_enabled {
			let mempool = self.client.get_txpool_content().await;

			let mut transactions = Vec::new();
			transactions
				.extend(mempool.queued.get(&self.client.address()).cloned().unwrap_or_default());
			transactions
				.extend(mempool.pending.get(&self.client.address()).cloned().unwrap_or_default());

			for (_nonce, transaction) in transactions {
				self.try_send_transaction(EventMessage::new(
					self.stuck_transaction_to_transaction_request(&transaction).await,
					EventMetadata::Flush(FlushMetadata::new()),
					false,
					false,
				))
				.await;
			}
		}
	}

	pub async fn stuck_transaction_to_transaction_request(
		&self,
		transaction: &Transaction,
	) -> TransactionRequest {
		TransactionRequest::default()
			.nonce(transaction.nonce)
			.from(transaction.from)
			.gas_price(self.get_gas_price_for_retry(transaction.gas_price.unwrap()).await)
			.to(transaction.to.unwrap_or_default())
			.value(transaction.value)
			.data(transaction.input.clone())
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
		if status.is_zero() && self.debug_mode {
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
		log::error!(
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
			sentry::Level::Error,
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
		if self.debug_mode {
			log::warn!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚠️  Warning! Error encountered during gas estimation: {}, Retries left: {:?}, Error: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata,
				msg.retries_remaining - 1,
				error.to_string()
			);
			sentry::capture_message(
				format!(
					"[{}]-[{}] ⚠️  Warning! Error encountered during gas estimation: {}, Retries left: {:?}, Error: {}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					msg.metadata,
					msg.retries_remaining - 1,
					error
				)
				.as_str(),
				sentry::Level::Warning,
			);
		}
		self.retry_transaction(msg).await;
	}

	/// Get current network's gas price
	async fn get_gas_price(&self) -> U256 {
		match self.middleware.get_gas_price().await {
			Ok(gas_price) => gas_price,
			Err(error) =>
				self.handle_failed_get_gas_price(DEFAULT_CALL_RETRIES, error.to_string()).await,
		}
	}

	/// Handles the failed get_gas_price().
	async fn handle_failed_get_gas_price(&self, retries_remaining: u8, error: String) -> U256 {
		let mut retries = retries_remaining;
		let mut last_error = error;

		while retries > 0 {
			if self.debug_mode {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during get gas price, Retries left: {:?}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					retries - 1,
					last_error
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}] ⚠️  Warning! Error encountered during get gas price, Retries left: {:?}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						retries - 1,
						last_error
					)
					.as_str(),
					sentry::Level::Warning,
				);
			}

			match self.middleware.get_gas_price().await {
				Ok(gas_price) => return gas_price,
				Err(error) => {
					sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					retries -= 1;
					last_error = error.to_string();
				},
			}
		}

		panic!(
			"[{}]-[{}] Error on call get_gas_price(): {:?}",
			self.client.get_chain_name(),
			SUB_LOG_TARGET,
			last_error,
		);
	}

	/// Sends the consumed transaction request to the connected chain. The transaction request will
	/// be re-published to the event channel if the transaction fails to be mined in a block.
	async fn try_send_transaction(&self, mut msg: EventMessage) {
		if msg.retries_remaining == 0 {
			return
		}

		// sets a random delay on external chain transactions on first try
		if msg.give_random_delay && msg.retries_remaining == DEFAULT_TX_RETRIES {
			sleep(Duration::from_millis(generate_delay())).await;
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

		if let Some(_gas_price) = msg.tx_request.gas_price {
		} else {
			// set the gas price to be used
			msg.tx_request = msg.tx_request.gas_price(self.get_gas_price().await);
		}

		// check the txpool for transaction duplication prevention
		if !(self.is_duplicate_relay(&mut msg.tx_request, msg.check_mempool).await) {
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

	async fn get_gas_price_for_retry(&self, previous_gas_price: U256) -> U256 {
		let current_network_gas_price = self.get_gas_price().await;
		let escalated_gas_price =
			U256::from((previous_gas_price.as_u64() as f64 * 1.125).ceil() as u64);

		max(current_network_gas_price, escalated_gas_price)
	}

	/// Function that query mempool for check if the relay event that this relayer is about to send
	/// has already been processed by another relayer.
	async fn is_duplicate_relay(
		&self,
		tx_request: &mut TransactionRequest,
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
		let from = tx_request.from.as_ref().unwrap();
		let gas_price = tx_request.gas_price.unwrap();

		let mempool = self.client.get_txpool_content().await;
		for (_address, tx_map) in mempool.pending.iter().chain(mempool.queued.iter()) {
			for (_nonce, mempool_tx) in tx_map.iter() {
				if mempool_tx.to.unwrap_or_default() == *to && mempool_tx.input == *data {
					// Trying gas escalating is not duplicate action
					if mempool_tx.from == *from {
						tx_request.gas_price =
							Option::from(self.get_gas_price_for_retry(gas_price).await);

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
