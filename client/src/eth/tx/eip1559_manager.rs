use crate::eth::{
	traits::{TransactionManager, TransactionTask},
	Eip1559GasMiddleware, EthClient,
};

use async_trait::async_trait;
use br_primitives::{
	constants::{
		config::ETHEREUM_BLOCK_TIME,
		errors::{INSUFFICIENT_FUNDS, NETWORK_DOES_NOT_SUPPORT_EIP1559, PROVIDER_INTERNAL_ERROR},
		tx::{DEFAULT_TX_RETRIES, MAX_FEE_COEFFICIENT, MAX_PRIORITY_FEE_COEFFICIENT},
	},
	sub_display_format,
	tx::{TxRequest, TxRequestMessage},
};
use ethers::{
	middleware::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
	prelude::Transaction,
	providers::{JsonRpcClient, Middleware},
	types::{
		transaction::eip2718::TypedTransaction, BlockId, BlockNumber, Bytes,
		Eip1559TransactionRequest, U256,
	},
};
use sc_service::SpawnTaskHandle;
use std::{cmp::max, sync::Arc, time::Duration};
use tokio::{
	sync::{
		mpsc,
		mpsc::{UnboundedReceiver, UnboundedSender},
	},
	time::sleep,
};

use super::{generate_delay, TransactionMiddleware};

const SUB_LOG_TARGET: &str = "eip1559-tx-manager";

/// The essential task that sends eip1559 transactions asynchronously.
pub struct Eip1559TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: Arc<TransactionMiddleware<T>>,
	/// The receiver connected to the tx request channel.
	receiver: UnboundedReceiver<TxRequestMessage>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The minimum priority fee required.
	min_priority_fee: U256,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	duplicate_confirm_delay: Option<u64>,
	/// A handle for spawning transaction tasks in the service.
	tx_spawn_handle: SpawnTaskHandle,
}

impl<T: 'static + JsonRpcClient> Eip1559TransactionManager<T> {
	/// Instantiates a new `Eip1559TransactionManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		min_priority_fee: U256,
		duplicate_confirm_delay: Option<u64>,
		tx_spawn_handle: SpawnTaskHandle,
	) -> (Self, UnboundedSender<TxRequestMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<TxRequestMessage>();

		let middleware = Arc::new(
			client
				.get_provider()
				.wrap_into(|p| SignerMiddleware::new(p, client.wallet.signer.clone()))
				.wrap_into(|p| NonceManagerMiddleware::new(p, client.address())),
		);

		(
			Self {
				client,
				middleware,
				receiver,
				is_txpool_enabled: false,
				min_priority_fee,
				duplicate_confirm_delay,
				tx_spawn_handle,
			},
			sender,
		)
	}
}

#[async_trait]
impl<T: 'static + JsonRpcClient> TransactionManager<T> for Eip1559TransactionManager<T> {
	async fn run(&mut self) {
		self.initialize().await;

		while let Some(msg) = self.receiver.recv().await {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔖 Received transaction request: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata,
			);

			self.spawn_send_transaction(msg).await;
		}
	}

	async fn initialize(&mut self) {
		self.is_txpool_enabled = self.client.provider.txpool_content().await.is_ok();

		self.flush_stuck_transaction().await;
	}

	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}

	fn get_spawn_handle(&self) -> SpawnTaskHandle {
		self.tx_spawn_handle.clone()
	}

	async fn spawn_send_transaction(&self, msg: TxRequestMessage) {
		let task = Eip1559TransactionTask::new(
			self.get_client(),
			self.middleware.clone(),
			self.is_txpool_enabled(),
			self.min_priority_fee,
			self.duplicate_confirm_delay,
		);
		if msg.is_bootstrap {
			task.try_send_transaction(msg).await;
		} else {
			self.get_spawn_handle().spawn("send_transaction", None, async move {
				task.try_send_transaction(msg).await
			});
		}
	}

	fn is_txpool_enabled(&self) -> bool {
		self.is_txpool_enabled
	}

	async fn stuck_transaction_to_transaction_request(
		&self,
		transaction: &Transaction,
	) -> TxRequest {
		let current_fees = self.client.get_estimated_eip1559_fees().await;

		let mut request: Eip1559TransactionRequest = transaction.into();
		if let Some(prev_max_fee_per_gas) = transaction.max_fee_per_gas {
			let prev_max_priority_fee_per_gas = transaction.max_priority_fee_per_gas.unwrap();

			let new_max_fee_per_gas = max(prev_max_fee_per_gas, current_fees.0);
			let new_priority_fee = max(
				prev_max_priority_fee_per_gas * MAX_PRIORITY_FEE_COEFFICIENT,
				max(current_fees.1, self.min_priority_fee),
			);

			request = request
				.max_fee_per_gas(new_max_fee_per_gas.saturating_add(new_priority_fee))
				.max_priority_fee_per_gas(new_priority_fee);
		} else {
			let prev_gas_price_escalated =
				U256::from((transaction.gas_price.unwrap().as_u64() as f64 * 1.125).ceil() as u64);

			if let Some(pending_block) =
				self.client.get_block(BlockId::Number(BlockNumber::Pending)).await
			{
				let pending_base_fee =
					pending_block.base_fee_per_gas.expect(NETWORK_DOES_NOT_SUPPORT_EIP1559);
				if prev_gas_price_escalated > pending_base_fee + current_fees.1 {
					request = request
						.max_fee_per_gas(prev_gas_price_escalated)
						.max_priority_fee_per_gas(prev_gas_price_escalated - pending_base_fee);
				} else {
					request = request
						.max_fee_per_gas(current_fees.0)
						.max_priority_fee_per_gas(current_fees.1);
				}
			} else {
				panic!(
					"[{}]-[{}]-[{}] {} [method: get_block]",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					PROVIDER_INTERNAL_ERROR
				);
			}
		};

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
}

/// The transaction task for Eip1559 transactions.
pub struct Eip1559TransactionTask<T> {
	/// The ethereum client for the connected chain.
	client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: Arc<TransactionMiddleware<T>>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The minimum priority fee required.
	min_priority_fee: U256,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	duplicate_confirm_delay: Option<u64>,
}

impl<T: JsonRpcClient> Eip1559TransactionTask<T> {
	/// Build an Eip1559 transaction task.
	pub fn new(
		client: Arc<EthClient<T>>,
		middleware: Arc<TransactionMiddleware<T>>,
		is_txpool_enabled: bool,
		min_priority_fee: U256,
		duplicate_confirm_delay: Option<u64>,
	) -> Self {
		Self { client, middleware, is_txpool_enabled, min_priority_fee, duplicate_confirm_delay }
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> TransactionTask<T> for Eip1559TransactionTask<T> {
	fn is_txpool_enabled(&self) -> bool {
		self.is_txpool_enabled
	}

	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}

	fn duplicate_confirm_delay(&self) -> Duration {
		Duration::from_millis(self.duplicate_confirm_delay.unwrap_or(ETHEREUM_BLOCK_TIME * 1000))
	}

	fn debug_mode(&self) -> bool {
		self.client.debug_mode
	}

	async fn try_send_transaction(&self, mut msg: TxRequestMessage) {
		msg.tx_request = TxRequest::Eip1559(msg.tx_request.to_eip1559());

		if msg.retries_remaining == 0 {
			return;
		}

		// sets a random delay on external chain transactions on first try
		if msg.give_random_delay && msg.retries_remaining == DEFAULT_TX_RETRIES {
			sleep(Duration::from_millis(generate_delay())).await;
		}

		// set transaction `from` field
		msg.tx_request.from(self.client.address());

		// set transaction gas fee parameters
		let fees = self.client.get_estimated_eip1559_fees().await;
		let priority_fee = max(fees.1, self.min_priority_fee);
		let max_fee_per_gas =
			fees.0.saturating_add(priority_fee).saturating_mul(MAX_FEE_COEFFICIENT.into());
		msg.tx_request.max_fee_per_gas(max_fee_per_gas);
		msg.tx_request.max_priority_fee_per_gas(priority_fee);

		match msg.tx_request.get_gas() {
			None => {
				// estimate the gas amount to be used
				let estimated_gas =
					match self.middleware.estimate_gas(&msg.tx_request.to_typed(), None).await {
						Ok(estimated_gas) => {
							br_metrics::increase_rpc_calls(&self.client.get_chain_name());
							U256::from(
								(estimated_gas.as_u64() as f64 * msg.gas_coefficient.into_f64())
									.ceil() as u64,
							)
						},
						Err(error) => {
							return self
								.handle_failed_gas_estimation(SUB_LOG_TARGET, msg, &error)
								.await
						},
					};
				msg.tx_request.gas(estimated_gas);
			},
			Some(_) => {},
		}

		// check the txpool for transaction duplication prevention
		if !self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await {
			// no duplication found
			if !self
				.is_sufficient_funds(max_fee_per_gas, msg.tx_request.get_gas().unwrap())
				.await
			{
				panic!(
					"[{}]-[{}]-[{}] {}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					INSUFFICIENT_FUNDS,
				);
			}

			let result = self.middleware.send_transaction(msg.tx_request.to_eip1559(), None).await;
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			match result {
				Ok(pending_tx) => match pending_tx.await {
					Ok(receipt) => {
						if let Some(receipt) = receipt {
							self.handle_success_tx_receipt(SUB_LOG_TARGET, receipt, msg.metadata);
						} else {
							self.handle_failed_tx_receipt(SUB_LOG_TARGET, msg).await;
						}
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
