use crate::eth::{
	EthClient, EventMessage, LegacyGasMiddleware, TransactionManager, TxRequest, DEFAULT_TX_RETRIES,
};
use async_trait::async_trait;
use br_primitives::{
	constants::{DEFAULT_ESCALATE_PERCENTAGE, DEFAULT_MIN_GAS_PRICE},
	eth::ETHEREUM_BLOCK_TIME,
	sub_display_format, INSUFFICIENT_FUNDS, NETWORK_DOES_NOT_SUPPORT_EIP1559,
	PROVIDER_INTERNAL_ERROR,
};
use ethers::{
	middleware::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::LocalWallet,
	types::{BlockId, BlockNumber, Transaction, TransactionRequest, U256},
};
use sc_service::SpawnTaskHandle;
use std::{sync::Arc, time::Duration};
use tokio::{
	sync::{
		mpsc,
		mpsc::{UnboundedReceiver, UnboundedSender},
	},
	time::sleep,
};

use super::{generate_delay, TransactionTask};

const SUB_LOG_TARGET: &str = "legacy-tx-manager";

type LegacyMiddleware<T> = NonceManagerMiddleware<SignerMiddleware<Arc<Provider<T>>, LocalWallet>>;

/// The essential task that sends legacy transactions asynchronously.
pub struct LegacyTransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: Arc<LegacyMiddleware<T>>,
	/// The receiver connected to the event channel.
	receiver: UnboundedReceiver<EventMessage>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The flag whether if the gas price will be initially escalated. The `escalate_percentage`
	/// will be used on escalation. This will only have effect on legacy transactions. (default:
	/// false)
	is_initially_escalated: bool,
	/// The coefficient used on transaction gas price escalation (default: 1.15)
	gas_price_coefficient: f64,
	/// The minimum value use for gas_price. (default: 0)
	min_gas_price: U256,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	duplicate_confirm_delay: Option<u64>,
	/// A handle for spawning transaction tasks in the service.
	tx_spawn_handle: SpawnTaskHandle,
}

impl<T: 'static + JsonRpcClient> LegacyTransactionManager<T> {
	/// Instantiates a new `LegacyTransactionManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		escalate_percentage: Option<f64>,
		min_gas_price: Option<u64>,
		is_initially_escalated: bool,
		duplicate_confirm_delay: Option<u64>,
		tx_spawn_handle: SpawnTaskHandle,
	) -> (Self, UnboundedSender<EventMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<EventMessage>();

		let gas_price_coefficient = {
			if let Some(escalate_percentage) = escalate_percentage {
				1.0 + (escalate_percentage / 100.0)
			} else {
				1.0 + (DEFAULT_ESCALATE_PERCENTAGE / 100.0)
			}
		};

		let middleware = client
			.get_provider()
			.wrap_into(|p| SignerMiddleware::new(p, client.wallet.signer.clone()))
			.wrap_into(|p| NonceManagerMiddleware::new(p, client.address()));

		(
			Self {
				client,
				middleware: Arc::new(middleware),
				receiver,
				is_txpool_enabled: false,
				is_initially_escalated,
				gas_price_coefficient,
				min_gas_price: U256::from(min_gas_price.unwrap_or(DEFAULT_MIN_GAS_PRICE)),
				duplicate_confirm_delay,
				tx_spawn_handle,
			},
			sender,
		)
	}
}

#[async_trait]
impl<T: 'static + JsonRpcClient> TransactionManager<T> for LegacyTransactionManager<T> {
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

	async fn spawn_send_transaction(&self, msg: EventMessage) {
		let task = LegacyTransactionTask::new(
			self.get_client(),
			self.middleware.clone(),
			self.is_txpool_enabled(),
			self.is_initially_escalated,
			self.gas_price_coefficient,
			self.min_gas_price,
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
		let request: TransactionRequest = transaction.into();

		return if let Some(gas_price) = transaction.gas_price {
			TxRequest::Legacy(
				request.gas_price(
					self.client
						.get_gas_price_for_retry(
							gas_price,
							self.gas_price_coefficient,
							self.min_gas_price,
						)
						.await,
				),
			)
		} else {
			let prev_priority_fee_per_gas = transaction.max_priority_fee_per_gas.unwrap();

			if let Some(pending_block) =
				self.client.get_block(BlockId::Number(BlockNumber::Pending)).await
			{
				let pending_base_fee =
					pending_block.base_fee_per_gas.expect(NETWORK_DOES_NOT_SUPPORT_EIP1559);

				TxRequest::Legacy(
					request.gas_price(
						self.client
							.get_gas_price_for_retry(
								prev_priority_fee_per_gas + pending_base_fee,
								self.gas_price_coefficient,
								self.min_gas_price,
							)
							.await,
					),
				)
			} else {
				panic!(
					"[{}]-[{}]-[{}] {} [method: get_block]",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					PROVIDER_INTERNAL_ERROR,
				);
			}
		};
	}
}

/// The transaction task for Legacy transactions.
pub struct LegacyTransactionTask<T> {
	/// The ethereum client for the connected chain.
	client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: Arc<LegacyMiddleware<T>>,
	/// The flag whether the client has enabled txpool namespace.
	is_txpool_enabled: bool,
	/// The flag whether if the gas price will be initially escalated. The `escalate_percentage`
	/// will be used on escalation. This will only have effect on legacy transactions. (default:
	/// false)
	is_initially_escalated: bool,
	/// The coefficient used on transaction gas price escalation (default: 1.15)
	gas_price_coefficient: f64,
	/// The minimum value used for gas_price. (default: 0)
	min_gas_price: U256,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	duplicate_confirm_delay: Option<u64>,
}

impl<T: JsonRpcClient> LegacyTransactionTask<T> {
	/// Build an Legacy transaction task.
	pub fn new(
		client: Arc<EthClient<T>>,
		middleware: Arc<LegacyMiddleware<T>>,
		is_txpool_enabled: bool,
		is_initially_escalated: bool,
		gas_price_coefficient: f64,
		min_gas_price: U256,
		duplicate_confirm_delay: Option<u64>,
	) -> Self {
		Self {
			client,
			middleware,
			is_txpool_enabled,
			is_initially_escalated,
			gas_price_coefficient,
			min_gas_price,
			duplicate_confirm_delay,
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> TransactionTask<T> for LegacyTransactionTask<T> {
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

	async fn try_send_transaction(&self, mut msg: EventMessage) {
		if msg.retries_remaining == 0 {
			return;
		}

		// sets a random delay on external chain transactions on first try
		if msg.give_random_delay && msg.retries_remaining == DEFAULT_TX_RETRIES {
			sleep(Duration::from_millis(generate_delay())).await;
		}

		// set transaction `from` field
		msg.tx_request.from(self.client.address());

		// set transaction `gas_price` field
		let mut gas_price = self.client.get_gas_price().await;
		if msg.tx_request.get_gas_price().unwrap_or(U256::zero()) == U256::zero() {
			if self.is_initially_escalated {
				gas_price = self
					.client
					.get_gas_price_for_escalation(
						gas_price,
						self.gas_price_coefficient,
						self.min_gas_price,
					)
					.await;
				msg.tx_request.gas_price(gas_price);
			}
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
				Err(error) => {
					return self.handle_failed_gas_estimation(SUB_LOG_TARGET, msg, &error).await
				},
			};
		msg.tx_request.gas(estimated_gas);

		// check the txpool for transaction duplication prevention
		if !self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await {
			if !self.is_sufficient_funds(gas_price, estimated_gas).await {
				panic!(
					"[{}]-[{}]-[{}] {}",
					&self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					INSUFFICIENT_FUNDS,
				);
			}

			let result = self.middleware.send_transaction(msg.tx_request.to_legacy(), None).await;
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			match result {
				Ok(pending_tx) => {
					let pending_hash = pending_tx.tx_hash();
					match pending_tx.await {
						Ok(receipt) => {
							if let Some(receipt) = receipt {
								self.handle_success_tx_receipt(
									SUB_LOG_TARGET,
									receipt,
									msg.metadata,
								)
							} else {
								// if 3 blocks passed since send_transaction, but the receipt has not come out,
								let pending_tx =
									self.get_client().get_transaction(pending_hash).await.unwrap();
								msg.tx_request.nonce(Some(pending_tx.nonce));
								msg.tx_request.gas_price(U256::from(
									(pending_tx.gas_price.unwrap().as_u64() as f64
										* self.gas_price_coefficient)
										.ceil() as u64,
								));
								self.handle_stalled_tx(SUB_LOG_TARGET, msg, pending_hash).await;
							}
						},
						Err(error) => {
							self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await
						},
					}
				},
				Err(error) => self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await,
			}
		}
	}
}
