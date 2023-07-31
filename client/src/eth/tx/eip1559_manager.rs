use crate::eth::{
	generate_delay, EthClient, EventMessage, TransactionManager, TxRequest, DEFAULT_CALL_RETRIES,
	DEFAULT_CALL_RETRY_INTERVAL_MS, DEFAULT_TX_RETRIES, MAX_FEE_COEFFICIENT,
	MAX_PRIORITY_FEE_COEFFICIENT,
};
use async_trait::async_trait;
use br_primitives::sub_display_format;
use ethers::{
	middleware::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
	prelude::Transaction,
	providers::{JsonRpcClient, Middleware, Provider},
	signers::LocalWallet,
	types::{transaction::eip2718::TypedTransaction, Bytes, Eip1559TransactionRequest, U256},
};
use std::{cmp::max, sync::Arc, time::Duration};
use tokio::{
	sync::{
		mpsc,
		mpsc::{UnboundedReceiver, UnboundedSender},
	},
	time::sleep,
};

const SUB_LOG_TARGET: &str = "eip1559-transaction-manager";

/// The essential task that sends eip1559 transactions asynchronously.
pub struct Eip1559TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: NonceManagerMiddleware<SignerMiddleware<Arc<Provider<T>>, LocalWallet>>,
	/// The receiver connected to the event channel.
	receiver: UnboundedReceiver<EventMessage>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged.
	debug_mode: bool,
	/// The minimum priority fee required.
	min_priority_fee: U256,
}

impl<T: 'static + JsonRpcClient> Eip1559TransactionManager<T> {
	/// Instantiates a new `Eip1559TransactionManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		debug_mode: bool,
		min_priority_fee: U256,
	) -> (Self, UnboundedSender<EventMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<EventMessage>();

		let middleware = client
			.get_provider()
			.wrap_into(|p| SignerMiddleware::new(p, client.wallet.signer.clone()))
			.wrap_into(|p| NonceManagerMiddleware::new(p, client.address()));

		(
			Self {
				client,
				middleware,
				receiver,
				is_txpool_enabled: false,
				debug_mode,
				min_priority_fee,
			},
			sender,
		)
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
}

#[async_trait]
impl<T: 'static + JsonRpcClient> TransactionManager<T> for Eip1559TransactionManager<T> {
	fn is_txpool_enabled(&self) -> bool {
		self.is_txpool_enabled
	}

	fn debug_mode(&self) -> bool {
		self.debug_mode
	}

	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}

	async fn initialize(&mut self) {
		self.is_txpool_enabled = self.client.provider.txpool_status().await.is_ok();

		self.flush_stuck_transaction().await;
	}

	async fn stuck_transaction_to_transaction_request(
		&self,
		transaction: &Transaction,
	) -> TxRequest {
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
	}

	async fn run(&mut self) {
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

	async fn try_send_transaction(&self, mut msg: EventMessage) {
		if msg.retries_remaining == 0 {
			return
		}

		// sets a random delay on external chain transactions on first try
		if msg.give_random_delay && msg.retries_remaining == DEFAULT_TX_RETRIES {
			sleep(Duration::from_millis(generate_delay())).await;
		}

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
				Err(error) =>
					return self.handle_failed_gas_estimation(SUB_LOG_TARGET, msg, &error).await,
			};
		msg.tx_request = msg.tx_request.gas(estimated_gas);

		// check the txpool for transaction duplication prevention
		if !(self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await) {
			// no duplication found
			let fees = self.get_estimated_eip1559_fees().await;
			let priority_fee = max(fees.1, self.min_priority_fee);

			let result = self
				.middleware
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
				.await;
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			match result {
				Ok(pending_tx) => match pending_tx.await {
					Ok(receipt) =>
						if let Some(receipt) = receipt {
							self.handle_success_tx_receipt(
								SUB_LOG_TARGET,
								Some(receipt),
								msg.metadata,
							);
						} else {
							self.handle_failed_tx_receipt(SUB_LOG_TARGET, msg).await;
						},
					Err(error) => {
						self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await;
					},
				},
				Err(error) => {
					self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await;
				},
			}
		}
	}
}
