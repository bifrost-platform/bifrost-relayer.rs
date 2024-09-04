use br_primitives::{
	constants::errors::INVALID_PROVIDER_URL, substrate::CustomConfig, tx::XtRequestMessage,
	utils::sub_display_format,
};

use ethers::providers::JsonRpcClient;
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use subxt::blocks::BlockRef;
use subxt::client::{OfflineClientT, OnlineClientT};
use subxt::config::DefaultExtrinsicParamsBuilder;
use subxt::tx::Signer;
use subxt::OnlineClient;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::{eth::EthClient, substrate::traits::ExtrinsicTask as ExtrinsicTaskT};

const SUB_LOG_TARGET: &str = "extrinsic-manager";

/// The essential task that sends extrinsics asynchronously.
pub struct ExtrinsicManager<T, S> {
	/// The substrate client.
	sub_client: Option<OnlineClient<CustomConfig>>,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T, S>>,
	/// The receiver connected to the tx request channel.
	receiver: UnboundedReceiver<XtRequestMessage>,
	/// A handle for spawning transaction tasks in the service.
	xt_spawn_handle: SpawnTaskHandle,
}

impl<T, S> ExtrinsicManager<T, S>
where
	T: 'static + JsonRpcClient,
	S: Signer<CustomConfig> + 'static + Send + Sync,
{
	/// Instantiates a new `ExtrinsicManager`.
	pub fn new(
		bfc_client: Arc<EthClient<T, S>>,
		xt_spawn_handle: SpawnTaskHandle,
	) -> (Self, UnboundedSender<XtRequestMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<XtRequestMessage>();
		(Self { sub_client: None, bfc_client, receiver, xt_spawn_handle }, sender)
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
			OnlineClient::<CustomConfig>::from_url(url.as_str())
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
				"-[{}] 🔖 Received extrinsic request: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata,
			);

			self.spawn_send_transaction(msg).await;
		}
	}

	/// Spawn a transaction task and try sending the transaction.
	pub async fn spawn_send_transaction(&self, msg: XtRequestMessage) {
		if let Some(sub_client) = &self.sub_client {
			let task = ExtrinsicTask::new(Arc::new(sub_client.clone()), self.bfc_client.clone());
			self.xt_spawn_handle
				.spawn("send_extrinsic", None, async move { task.try_send_extrinsic(msg).await });
		}
	}
}

/// The transaction task for extrinsics.
pub struct ExtrinsicTask<T, S> {
	/// The substrate client.
	sub_client: Arc<OnlineClient<CustomConfig>>,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T, S>>,
}

impl<T, S> ExtrinsicTask<T, S>
where
	T: JsonRpcClient,
	S: Signer<CustomConfig>,
{
	/// Build an `ExtrinsicTask` instance.
	pub fn new(
		sub_client: Arc<OnlineClient<CustomConfig>>,
		bfc_client: Arc<EthClient<T, S>>,
	) -> Self {
		Self { sub_client, bfc_client }
	}
}

#[async_trait::async_trait]
impl<T, S> ExtrinsicTaskT<T, S> for ExtrinsicTask<T, S>
where
	T: 'static + JsonRpcClient,
	S: Signer<CustomConfig> + Send + Sync,
{
	fn get_sub_client(&self) -> Arc<OnlineClient<CustomConfig>> {
		self.sub_client.clone()
	}

	fn get_bfc_client(&self) -> Arc<EthClient<T, S>> {
		self.bfc_client.clone()
	}

	async fn try_send_extrinsic(&self, msg: XtRequestMessage) {
		if msg.retries_remaining == 0 {
			return;
		}

		let progress = self
			.sub_client
			.tx()
			.sign_and_submit_then_watch_no_refine(
				&msg.call,
				&self.bfc_client.wallet.subxt_signer,
				DefaultExtrinsicParamsBuilder::new()
					.nonce(self.bfc_client.middleware.next().as_u64())
					.build(),
			)
			.await;
		self.bfc_client.middleware.next();

		match progress {
			Ok(progress) => match progress.wait_for_finalized_success().await {
				Ok(events) => {
					self.handle_success_xt_request(SUB_LOG_TARGET, events, msg.metadata).await
				},
				Err(error) => self.handle_failed_xt_request(SUB_LOG_TARGET, msg, &error).await,
			},
			Err(error) => self.handle_failed_xt_request(SUB_LOG_TARGET, msg, &error).await,
		}
	}
}
