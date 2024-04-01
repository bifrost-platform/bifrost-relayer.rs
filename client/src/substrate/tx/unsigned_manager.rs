use br_primitives::{
	constants::errors::INVALID_PROVIDER_URL, sub_display_format, substrate::CustomConfig,
	tx::XtRequestMessage,
};

use ethers::providers::JsonRpcClient;
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use subxt::{tx::TxPayload, OnlineClient};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::{eth::EthClient, substrate::traits::ExtrinsicTask};

const SUB_LOG_TARGET: &str = "unsigned-tx-manager";

/// The essential task that sends unsigned transactions asynchronously.
pub struct UnsignedTransactionManager<T, Call> {
	/// The substrate client.
	sub_client: Option<OnlineClient<CustomConfig>>,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T>>,
	/// The receiver connected to the tx request channel.
	receiver: UnboundedReceiver<XtRequestMessage<Call>>,
	/// A handle for spawning transaction tasks in the service.
	xt_spawn_handle: SpawnTaskHandle,
}

impl<T, Call> UnsignedTransactionManager<T, Call>
where
	T: 'static + JsonRpcClient,
	Call: 'static + TxPayload + Send,
{
	/// Instantiates a new `UnsignedTransactionManager`.
	pub fn new(
		bfc_client: Arc<EthClient<T>>,
		xt_spawn_handle: SpawnTaskHandle,
	) -> (Self, UnboundedSender<XtRequestMessage<Call>>) {
		let (sender, receiver) = mpsc::unbounded_channel::<XtRequestMessage<Call>>();
		(Self { sub_client: None, bfc_client, receiver, xt_spawn_handle }, sender)
	}

	/// Initialize the substrate client.
	async fn initialize(&mut self) {
		self.sub_client = Some(
			OnlineClient::<CustomConfig>::from_url(&self.bfc_client.metadata.url)
				.await
				.expect(INVALID_PROVIDER_URL),
		);
	}

	/// Starts the transaction manager. Listens to every new consumed tx request message.
	pub async fn run(&mut self) {
		self.initialize().await;

		while let Some(msg) = self.receiver.recv().await {
			log::info!(
				target: &self.bfc_client.get_chain_name(),
				"-[{}] 🔖 Received unsigned transaction request: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata,
			);

			self.spawn_send_transaction(msg).await;
		}
	}

	/// Spawn a transaction task and try sending the transaction.
	pub async fn spawn_send_transaction(&self, msg: XtRequestMessage<Call>) {
		if let Some(sub_client) = &self.sub_client {
			let task =
				UnsignedTransactionTask::new(Arc::new(sub_client.clone()), self.bfc_client.clone());
			self.xt_spawn_handle.spawn("send_unsigned_transaction", None, async move {
				task.try_send_unsigned_transaction(msg).await
			});
		}
	}
}

/// The transaction task for unsigned transactions.
pub struct UnsignedTransactionTask<T> {
	/// The substrate client.
	sub_client: Arc<OnlineClient<CustomConfig>>,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T>>,
}

impl<T: JsonRpcClient> UnsignedTransactionTask<T> {
	/// Build an unsigned transaction task instance.
	pub fn new(sub_client: Arc<OnlineClient<CustomConfig>>, bfc_client: Arc<EthClient<T>>) -> Self {
		Self { sub_client, bfc_client }
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> ExtrinsicTask<T> for UnsignedTransactionTask<T> {
	fn get_bfc_client(&self) -> Arc<EthClient<T>> {
		self.bfc_client.clone()
	}

	fn get_sub_client(&self) -> Arc<OnlineClient<CustomConfig>> {
		self.sub_client.clone()
	}

	async fn try_send_unsigned_transaction<Call: TxPayload + Send>(
		&self,
		msg: XtRequestMessage<Call>,
	) {
		match self.sub_client.tx().create_unsigned(&msg.call) {
			Ok(xt) => match xt.submit_and_watch().await {
				Ok(progress) => match progress.wait_for_finalized_success().await {
					Ok(events) => {
						self.handle_success_tx_request(SUB_LOG_TARGET, events, msg.metadata).await
					},
					Err(error) => self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await,
				},
				Err(error) => self.handle_failed_tx_request(SUB_LOG_TARGET, msg, &error).await,
			},
			Err(error) => self.handle_failed_tx_creation(SUB_LOG_TARGET, msg, &error).await,
		}
	}
}
