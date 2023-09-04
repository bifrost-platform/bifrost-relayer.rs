use crate::eth::{
	generate_delay, EthClient, EventMessage, TransactionManager, TxRequest, DEFAULT_CALL_RETRIES,
	DEFAULT_CALL_RETRY_INTERVAL_MS, DEFAULT_TX_RETRIES,
};
use async_trait::async_trait;
use br_primitives::{eth::ETHEREUM_BLOCK_TIME, sub_display_format, NETWORK_NOT_SUPPORT_EIP1559};
use ethers::{
	middleware::{
		gas_escalator::{Frequency, GasEscalatorMiddleware, GeometricGasPrice},
		MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware,
	},
	providers::{JsonRpcClient, Middleware, Provider},
	signers::LocalWallet,
	types::{BlockId, BlockNumber, Transaction, TransactionRequest, U256},
};
use std::{cmp::max, sync::Arc, time::Duration};
use tokio::{
	sync::{
		mpsc,
		mpsc::{UnboundedReceiver, UnboundedSender},
	},
	time::sleep,
};

const SUB_LOG_TARGET: &str = "legacy-tx-manager";

type LegacyMiddleware<T> = NonceManagerMiddleware<
	SignerMiddleware<Arc<GasEscalatorMiddleware<Arc<Provider<T>>>>, LocalWallet>,
>;

/// The essential task that sends legacy transactions asynchronously.
pub struct LegacyTransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The client signs transaction for the connected chain with local nonce manager.
	middleware: LegacyMiddleware<T>,
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
	/// The flag whether debug mode is enabled. If enabled, certain errors will be logged such as
	/// gas estimation failures.
	debug_mode: bool,
	/// If first relay transaction is stuck in mempool after waiting for this amount of time(ms),
	/// ignore duplicate prevent logic. (default: 12s)
	duplicate_confirm_delay: Option<u64>,
}

impl<T: 'static + JsonRpcClient> LegacyTransactionManager<T> {
	/// Instantiates a new `LegacyTransactionManager` instance.
	pub fn new(
		client: Arc<EthClient<T>>,
		debug_mode: bool,
		escalate_interval: Option<u64>,
		escalate_percentage: Option<f64>,
		min_gas_price: Option<u64>,
		is_initially_escalated: bool,
		duplicate_confirm_delay: Option<u64>,
	) -> (Self, UnboundedSender<EventMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<EventMessage>();

		let mut gas_price_coefficient = 1.15;
		if let Some(escalate_percentage) = escalate_percentage {
			gas_price_coefficient = 1.0 + (escalate_percentage / 100.0);
		}

		let escalator = GeometricGasPrice::new(
			gas_price_coefficient,
			escalate_interval.unwrap_or(ETHEREUM_BLOCK_TIME),
			None::<u64>,
		);
		let middleware = client
			.get_provider()
			.wrap_into(|p| Arc::new(GasEscalatorMiddleware::new(p, escalator, Frequency::PerBlock)))
			.wrap_into(|p| SignerMiddleware::new(p, client.wallet.signer.clone()))
			.wrap_into(|p| NonceManagerMiddleware::new(p, client.address()));

		(
			Self {
				client,
				middleware,
				receiver,
				is_txpool_enabled: false,
				is_initially_escalated,
				gas_price_coefficient,
				min_gas_price: U256::from(min_gas_price.unwrap_or(0)),
				debug_mode,
				duplicate_confirm_delay,
			},
			sender,
		)
	}

	/// Get gas_price for legacy retry transaction request.
	/// returns `max(current_network_gas_price,escalated_gas_price)`
	async fn get_gas_price_for_retry(&self, previous_gas_price: U256) -> U256 {
		let previous_gas_price = previous_gas_price.as_u64() as f64;

		let current_network_gas_price = match self.middleware.get_gas_price().await {
			Ok(gas_price) => {
				br_metrics::increase_rpc_calls(&self.client.get_chain_name());
				gas_price
			},
			Err(error) =>
				self.handle_failed_get_gas_price(DEFAULT_CALL_RETRIES, error.to_string()).await,
		};
		let escalated_gas_price =
			U256::from((previous_gas_price * self.gas_price_coefficient).ceil() as u64);

		max(max(current_network_gas_price, escalated_gas_price), self.min_gas_price)
	}

	/// Get gas_price for escalated legacy transaction request. This will be only used when
	/// `is_initially_escalated` is enabled.
	async fn get_gas_price_for_escalation(&self) -> U256 {
		let current_network_gas_price = match self.middleware.get_gas_price().await {
			Ok(gas_price) => {
				br_metrics::increase_rpc_calls(&self.client.get_chain_name());
				gas_price
			},
			Err(error) =>
				self.handle_failed_get_gas_price(DEFAULT_CALL_RETRIES, error.to_string()).await,
		};

		max(
			U256::from(
				(current_network_gas_price.as_u64() as f64 * self.gas_price_coefficient).ceil()
					as u64,
			),
			self.min_gas_price,
		)
	}

	/// Handles the failed gas price rpc request.
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

#[async_trait]
impl<T: 'static + JsonRpcClient> TransactionManager<T> for LegacyTransactionManager<T> {
	fn is_txpool_enabled(&self) -> bool {
		self.is_txpool_enabled
	}
	fn debug_mode(&self) -> bool {
		self.debug_mode
	}
	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}

	fn duplicate_confirm_delay(&self) -> Duration {
		Duration::from_millis(self.duplicate_confirm_delay.unwrap_or(ETHEREUM_BLOCK_TIME * 1000))
	}

	async fn initialize(&mut self) {
		self.is_txpool_enabled = self.client.provider.txpool_content().await.is_ok();

		self.flush_stuck_transaction().await;
	}

	async fn stuck_transaction_to_transaction_request(
		&self,
		transaction: &Transaction,
	) -> TxRequest {
		let request: TransactionRequest = transaction.into();

		return if let Some(gas_price) = transaction.gas_price {
			TxRequest::Legacy(request.gas_price(self.get_gas_price_for_retry(gas_price).await))
		} else {
			let prev_priority_fee_per_gas = transaction.max_priority_fee_per_gas.unwrap();

			if let Some(pending_block) =
				self.client.get_block(BlockId::Number(BlockNumber::Pending)).await
			{
				let pending_base_fee =
					pending_block.base_fee_per_gas.expect(NETWORK_NOT_SUPPORT_EIP1559);

				TxRequest::Legacy(
					request.gas_price(
						self.get_gas_price_for_retry(prev_priority_fee_per_gas + pending_base_fee)
							.await,
					),
				)
			} else {
				panic!(
					"[{}]-[{}] Error on call get_block rpc",
					self.client.get_chain_name(),
					SUB_LOG_TARGET,
				)
			}
		}
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
				Err(error) =>
					return self.handle_failed_gas_estimation(SUB_LOG_TARGET, msg, &error).await,
			};
		msg.tx_request = msg.tx_request.gas(estimated_gas);

		// check the txpool for transaction duplication prevention
		if !(self.is_duplicate_relay(&msg.tx_request, msg.check_mempool).await) {
			let mut tx = msg.tx_request.to_legacy();
			if self.is_initially_escalated {
				tx = tx.gas_price(self.get_gas_price_for_escalation().await);
			}
			let result = self.middleware.send_transaction(tx, None).await;
			br_metrics::increase_rpc_calls(&self.client.get_chain_name());

			match result {
				Ok(pending_tx) => match pending_tx.await {
					Ok(receipt) =>
						self.handle_success_tx_receipt(SUB_LOG_TARGET, receipt, msg.metadata),
					Err(error) => self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await,
				},
				Err(error) => self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await,
			}
		}
	}
}
