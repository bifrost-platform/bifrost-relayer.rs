use async_recursion::async_recursion;

use crate::eth::{FlushMetadata, TxRequest, DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS};
use br_primitives::{
	eth::{GasCoefficient, ETHEREUM_BLOCK_TIME},
	sub_display_format,
};
use ethers::{
	prelude::{
		gas_escalator::{Frequency, GasEscalatorMiddleware, GeometricGasPrice},
		NonceManagerMiddleware, SignerMiddleware,
	},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::{LocalWallet, Signer},
	types::{
		transaction::eip2718::TypedTransaction, BlockId, BlockNumber, Bytes,
		Eip1559TransactionRequest, Transaction, TransactionReceipt, TransactionRequest, U256,
	},
};
use rand::Rng;
use std::{cmp::max, error::Error, sync::Arc};
use tokio::{
	sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
	time::{sleep, Duration},
};

use super::{
	EthClient, EventMessage, EventMetadata, DEFAULT_TX_RETRIES, MAX_FEE_COEFFICIENT,
	MAX_PRIORITY_FEE_COEFFICIENT, RETRY_GAS_PRICE_COEFFICIENT,
};

pub type TransactionMiddleware<T> =
	NonceManagerMiddleware<SignerMiddleware<GasEscalatorMiddleware<Arc<Provider<T>>>, LocalWallet>>;

const SUB_LOG_TARGET: &str = "transaction-manager";

/// Generates a random delay that is ranged as 0 to 12000 milliseconds (in milliseconds).
fn generate_delay() -> u64 {
	rand::thread_rng().gen_range(0..=12000)
}

trait OnSuccessHandler {
	/// Handles the successfully mined transaction.
	fn handle_success_tx_receipt(&self, receipt: TransactionReceipt, metadata: EventMetadata);
}

#[async_trait::async_trait]
trait OnFailHandler {
	/// Handles the failed transaction receipt generation.
	async fn handle_failed_tx_receipt(&self, msg: EventMessage);

	/// Handles the failed transaction request.
	async fn handle_failed_tx_request<E: Error + Sync + ?Sized>(
		&self,
		msg: EventMessage,
		error: &E,
	);

	/// Handles the failed gas estimation.
	async fn handle_failed_gas_estimation<E: Error + Sync + ?Sized>(
		&self,
		msg: EventMessage,
		error: &E,
	);

	/// Handles the failed `get_estimated_eip1559_fees()`.
	async fn handle_failed_get_estimated_eip1559_fees(
		&self,
		retries_remaining: u8,
		error: String,
	) -> (U256, U256);

	/// Handles the failed `get_gas_price()`.
	async fn handle_failed_get_gas_price(&self, retries_remaining: u8, error: String) -> U256;
}

/// The essential task that sends asynchronous transactions.
pub struct TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: TransactionMiddleware<T>,
	/// The receiver connected to the event channel.
	receiver: UnboundedReceiver<EventMessage>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged.
	debug_mode: bool,
	/// If true, send transaction with EIP1559
	eip1559: bool,
	/// The minimum priority fee required.
	min_priority_fee: U256,
}

impl<T: 'static + JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `TransactionManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		debug_mode: bool,
		eip1559: bool,
		min_priority_fee: U256,
	) -> (Self, UnboundedSender<EventMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<EventMessage>();

		// Bumps transactions gas price in the background to avoid getting them stuck in the memory
		// pool.
		let geometric_gas_price = GeometricGasPrice::new(1.5, ETHEREUM_BLOCK_TIME, None::<u64>);
		let gas_escalator = GasEscalatorMiddleware::new(
			client.provider.clone(),
			geometric_gas_price,
			Frequency::Duration(ETHEREUM_BLOCK_TIME * 1000),
		);
		// Signs transactions locally, with a private key or a hardware wallet.
		let signer = SignerMiddleware::new(gas_escalator, client.wallet.signer.clone());
		// Manages nonces locally. Allows to sign multiple consecutive transactions without waiting
		// for them to hit the mempool.
		let middleware = NonceManagerMiddleware::new(signer, client.wallet.signer.address());

		(
			Self {
				client,
				middleware,
				receiver,
				is_txpool_enabled: false,
				debug_mode,
				eip1559,
				min_priority_fee,
			},
			sender,
		)
	}

	/// Initialize transaction manager.
	async fn initialize(&mut self) {
		// check txpool namespace available
		self.is_txpool_enabled = self.client.provider.txpool_status().await.is_ok();

		self.flush_stuck_transaction().await;
	}

	/// Flush all transaction from mempool.
	async fn flush_stuck_transaction(&self) {
		if self.is_txpool_enabled && !self.client.is_native {
			let mempool = self.client.get_txpool_content().await;
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			let mut transactions = Vec::new();
			transactions
				.extend(mempool.queued.get(&self.client.address()).cloned().unwrap_or_default());
			transactions
				.extend(mempool.pending.get(&self.client.address()).cloned().unwrap_or_default());

			for (_nonce, transaction) in transactions {
				self.try_send_transaction(EventMessage::new(
					self.stuck_transaction_to_transaction_request(&transaction).await,
					EventMetadata::Flush(FlushMetadata::default()),
					false,
					false,
					GasCoefficient::Low,
				))
				.await;
			}
		}
	}

	/// Converts stuck transaction to `TxRequest(TransactionRequest | Eip1559TransactionRequest)`
	pub async fn stuck_transaction_to_transaction_request(
		&self,
		transaction: &Transaction,
	) -> TxRequest {
		match self.eip1559 {
			true => {
				let mut request: Eip1559TransactionRequest = transaction.into();

				let current_fees = self.get_estimated_eip1559_fees().await;

				let new_max_fee_per_gas = max(request.max_fee_per_gas.unwrap(), current_fees.0);
				let new_priority_fee = max(
					request.max_priority_fee_per_gas.unwrap() * MAX_PRIORITY_FEE_COEFFICIENT,
					max(current_fees.1, self.min_priority_fee),
				);

				request = request
					.max_fee_per_gas(new_max_fee_per_gas.saturating_add(new_priority_fee))
					.max_priority_fee_per_gas(new_priority_fee);

				if self
					.middleware
					.estimate_gas(&TypedTransaction::Eip1559(request.clone()), None)
					.await
					.is_err()
				{
					request = request.to(self.client.address()).value(0).data(Bytes::default());
				}

				TxRequest::Eip1559(request)
			},
			false => {
				let request: TransactionRequest = transaction.into();
				let new_gas_price = self.get_gas_price_for_retry(request.gas_price.unwrap()).await;

				TxRequest::Legacy(request.gas_price(new_gas_price))
			},
		}
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

	/// Retry send_transaction() for failed transaction execution.
	#[async_recursion]
	async fn retry_transaction(&self, mut msg: EventMessage) {
		msg.build_retry_event();
		sleep(Duration::from_millis(msg.retry_interval)).await;
		self.try_send_transaction(msg).await;
	}

	/// Get current network's gas price (If eip1559 flag is true, returns pending block's base fee)
	async fn get_gas_price(&self) -> U256 {
		match self.eip1559 {
			true => self
				.client
				.get_block(BlockId::Number(BlockNumber::Pending))
				.await
				.unwrap()
				.base_fee_per_gas
				.unwrap(),
			false => match self.middleware.get_gas_price().await {
				Ok(gas_price) => {
					br_metrics::increase_rpc_calls(&self.client.get_chain_name());
					gas_price
				},
				Err(error) =>
					self.handle_failed_get_gas_price(DEFAULT_CALL_RETRIES, error.to_string()).await,
			},
		}
	}

	/// Gets a heuristic recommendation of max fee per gas and max priority fee per gas for EIP-1559
	/// compatible transactions.
	async fn get_estimated_eip1559_fees(&self) -> (U256, U256) {
		match self.middleware.estimate_eip1559_fees(None).await {
			Ok(fees) => {
				br_metrics::increase_rpc_calls(&self.client.get_chain_name());
				fees
			},
			Err(error) =>
				self.handle_failed_get_estimated_eip1559_fees(
					DEFAULT_CALL_RETRIES,
					error.to_string(),
				)
				.await,
		}
	}

	/// Get gas_price for legacy retry transaction request.
	/// returns `max(current_network_gas_price,escalated_gas_price)`
	async fn get_gas_price_for_retry(&self, previous_gas_price: U256) -> U256 {
		let previous_gas_price = previous_gas_price.as_u64() as f64;

		let current_network_gas_price = self.get_gas_price().await;
		let escalated_gas_price =
			U256::from((previous_gas_price * RETRY_GAS_PRICE_COEFFICIENT).ceil() as u64);

		max(current_network_gas_price, escalated_gas_price)
	}

	/// Sends the consumed transaction request to the connected chain. The transaction send will
	/// be retry if the transaction fails to be mined in a block.
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
		let estimated_gas =
			match self.middleware.estimate_gas(&msg.tx_request.to_typed(), None).await {
				Ok(estimated_gas) => {
					br_metrics::increase_rpc_calls(&self.client.get_chain_name());
					U256::from(
						(estimated_gas.as_u64() as f64 * msg.gas_coefficient.into_f64()).ceil()
							as u64,
					)
				},
				Err(error) => return self.handle_failed_gas_estimation(msg, &error).await,
			};
		msg.tx_request = msg.tx_request.gas(estimated_gas);

		// check the txpool for transaction duplication prevention
		if !(self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await) {
			// no duplication found
			let result = if self.eip1559 {
				let fees = self.get_estimated_eip1559_fees().await;
				let priority_fee = max(fees.1, self.min_priority_fee);
				self.middleware
					.send_transaction(
						msg.tx_request
							.to_eip1559()
							.max_fee_per_gas(
								fees.0
									.saturating_add(priority_fee)
									.saturating_mul(MAX_FEE_COEFFICIENT.into()),
							)
							.max_priority_fee_per_gas(priority_fee),
						None,
					)
					.await
			} else {
				self.middleware.send_transaction(msg.tx_request.to_legacy(), None).await
			};
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			match result {
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
			}
		}
	}

	/// Function that query mempool for check if the relay event that this relayer is about to send
	/// has already been processed by another relayer.
	async fn is_duplicate_relay(&self, tx_request: &TxRequest, check_mempool: bool) -> bool {
		// does not check the txpool if the following condition satisfies
		// 1. the txpool namespace is disabled for the client
		// 2. the txpool check flag is false
		// 3. the client is BIFROST (native)
		if !self.is_txpool_enabled || !check_mempool || self.client.is_native {
			return false
		}

		let (data, to, from) = match tx_request {
			TxRequest::Legacy(request) => (
				request.data.as_ref().unwrap(),
				request.to.as_ref().unwrap().as_address().unwrap(),
				request.from.as_ref().unwrap(),
			),
			TxRequest::Eip1559(request) => (
				request.data.as_ref().unwrap(),
				request.to.as_ref().unwrap().as_address().unwrap(),
				request.from.as_ref().unwrap(),
			),
		};

		let mempool = self.client.get_txpool_content().await;
		br_metrics::increase_rpc_calls(&self.client.get_chain_name());

		for (_address, tx_map) in mempool.pending.iter().chain(mempool.queued.iter()) {
			for (_nonce, mempool_tx) in tx_map.iter() {
				if mempool_tx.to.unwrap_or_default() == *to && mempool_tx.input == *data {
					// Trying gas escalating is not duplicate action
					if mempool_tx.from == *from {
						return false
					}
					return true
				}
			}
		}
		false
	}
}

impl<T: 'static + JsonRpcClient> OnSuccessHandler for TransactionManager<T> {
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
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during contract execution [execution reverted]. A prior transaction might have been already submitted: {}, {:?}-{:?}-{:?}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						metadata,
						receipt.block_number.unwrap(),
						status,
						receipt.transaction_hash
					)
					.as_str(),
					sentry::Level::Warning,
				);
		}
		br_metrics::set_payed_fees(&self.client.get_chain_name(), &receipt);
	}
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> OnFailHandler for TransactionManager<T> {
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
					"[{}]-[{}]-[{}] ♻️  The requested transaction failed to generate a receipt: {}, Retries left: {:?}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					msg.metadata,
					msg.retries_remaining - 1,
				)
			.as_str(),
			sentry::Level::Error,
		);
		self.retry_transaction(msg).await;
	}

	async fn handle_failed_tx_request<E>(&self, msg: EventMessage, error: &E)
	where
		E: Error + Sync + ?Sized,
	{
		log::error!(
			target: &self.client.get_chain_name(),
			"-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(SUB_LOG_TARGET),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		sentry::capture_message(
			format!(
				"[{}]-[{}]-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
				&self.client.get_chain_name(),
				SUB_LOG_TARGET,
				self.client.address(),
				msg.metadata,
				msg.retries_remaining - 1,
				error
			)
			.as_str(),
			sentry::Level::Error,
		);
		self.retry_transaction(msg).await;
	}

	async fn handle_failed_gas_estimation<E>(&self, msg: EventMessage, error: &E)
	where
		E: Error + Sync + ?Sized,
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
						"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during gas estimation: {}, Retries left: {:?}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
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

	async fn handle_failed_get_estimated_eip1559_fees(
		&self,
		retries_remaining: u8,
		error: String,
	) -> (U256, U256) {
		let mut retries = retries_remaining;
		let mut last_error = error;

		while retries > 0 {
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			if self.debug_mode {
				log::warn!(
					target: &self.client.get_chain_name(),
					"-[{}] ⚠️  Warning! Error encountered during get estimated eip1559 fees, Retries left: {:?}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					retries - 1,
					last_error
				);
				sentry::capture_message(
						format!(
							"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during get estimated eip1559 fees, Retries left: {:?}, Error: {}",
							&self.client.get_chain_name(),
							SUB_LOG_TARGET,
							self.client.address(),
							retries - 1,
							last_error
						)
						.as_str(),
						sentry::Level::Warning,
					);
			}

			match self.middleware.estimate_eip1559_fees(None).await {
				Ok(fees) => return fees,
				Err(error) => {
					sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					retries -= 1;
					last_error = error.to_string();
				},
			}
		}

		panic!(
			"[{}]-[{}] Error on call get_estimated_eip1559_fees(): {:?}",
			self.client.get_chain_name(),
			SUB_LOG_TARGET,
			last_error,
		);
	}

	async fn handle_failed_get_gas_price(&self, retries_remaining: u8, error: String) -> U256 {
		let mut retries = retries_remaining;
		let mut last_error = error;

		while retries > 0 {
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

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
							"[{}]-[{}]-[{}] ⚠️  Warning! Error encountered during get gas price, Retries left: {:?}, Error: {}",
							&self.client.get_chain_name(),
							SUB_LOG_TARGET,
							self.client.address(),
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
