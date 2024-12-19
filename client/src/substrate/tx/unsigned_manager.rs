use alloy::{
	network::AnyNetwork,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	transports::Transport,
};
use br_primitives::{
	constants::errors::INVALID_PROVIDER_URL,
	substrate::CustomConfig,
	tx::{XtRequest, XtRequestMessage},
	utils::sub_display_format,
};

use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use subxt::OnlineClient;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_stream::{wrappers::UnboundedReceiverStream, StreamExt};

use crate::{eth::EthClient, substrate::traits::ExtrinsicTask};

const SUB_LOG_TARGET: &str = "unsigned-tx-manager";

/// The essential task that sends unsigned transactions asynchronously.
pub struct UnsignedTransactionManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The substrate client.
	sub_client: Option<OnlineClient<CustomConfig>>,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<F, P, T>>,
	/// The receiver connected to the tx request channel.
	message_stream: UnboundedReceiverStream<XtRequestMessage>,
	/// A handle for spawning transaction tasks in the service.
	xt_spawn_handle: SpawnTaskHandle,
}

impl<F, P, T> UnsignedTransactionManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	/// Instantiates a new `UnsignedTransactionManager`.
	pub fn new(
		bfc_client: Arc<EthClient<F, P, T>>,
		xt_spawn_handle: SpawnTaskHandle,
	) -> (Self, UnboundedSender<XtRequestMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<XtRequestMessage>();
		(
			Self {
				sub_client: None,
				bfc_client,
				message_stream: UnboundedReceiverStream::new(receiver),
				xt_spawn_handle,
			},
			sender,
		)
	}

	/// Initialize the substrate client.
	async fn initialize(&mut self) {
		let mut url = self.bfc_client.get_url();
		if url.scheme() == "https" {
			url.set_scheme("wss").expect(INVALID_PROVIDER_URL);
		} else {
			url.set_scheme("ws").expect(INVALID_PROVIDER_URL);
		}

		self.sub_client = Some(
			OnlineClient::<CustomConfig>::from_insecure_url(url.as_str())
				.await
				.expect(INVALID_PROVIDER_URL),
		);
	}

	/// Starts the transaction manager. Listens to every new consumed tx request message.
	pub async fn run(&mut self) {
		self.initialize().await;

		while let Some(msg) = self.message_stream.next().await {
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
	pub async fn spawn_send_transaction(&self, msg: XtRequestMessage) {
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
pub struct UnsignedTransactionTask<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The substrate client.
	sub_client: Arc<OnlineClient<CustomConfig>>,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<F, P, T>>,
}

impl<F, P, T> UnsignedTransactionTask<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Build an unsigned transaction task instance.
	pub fn new(
		sub_client: Arc<OnlineClient<CustomConfig>>,
		bfc_client: Arc<EthClient<F, P, T>>,
	) -> Self {
		Self { sub_client, bfc_client }
	}
}

#[async_trait::async_trait]
impl<F, P, T> ExtrinsicTask<F, P, T> for UnsignedTransactionTask<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	fn get_sub_client(&self) -> Arc<OnlineClient<CustomConfig>> {
		self.sub_client.clone()
	}

	fn get_bfc_client(&self) -> Arc<EthClient<F, P, T>> {
		self.bfc_client.clone()
	}

	async fn try_send_unsigned_transaction(&self, msg: XtRequestMessage) {
		if msg.retries_remaining == 0 {
			return;
		}

		match self.sub_client.tx().create_unsigned::<XtRequest>(&msg.call) {
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
