use br_primitives::{sub_display_format, substrate::CustomConfig, tx::XtRequestMessage};
use ethers::providers::JsonRpcClient;
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use subxt::{tx::TxPayload, OnlineClient};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::{eth::EthClient, substrate::traits::ExtrinsicTask};

const SUB_LOG_TARGET: &str = "unsigned-tx-manager";

pub struct UnsignedTransactionManager<T, Call> {
	sub_client: Arc<OnlineClient<CustomConfig>>,
	eth_client: Arc<EthClient<T>>,
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
	pub fn new(
		sub_client: Arc<OnlineClient<CustomConfig>>,
		eth_client: Arc<EthClient<T>>,
		xt_spawn_handle: SpawnTaskHandle,
	) -> (Self, UnboundedSender<XtRequestMessage<Call>>) {
		let (sender, receiver) = mpsc::unbounded_channel::<XtRequestMessage<Call>>();
		(Self { sub_client, eth_client, receiver, xt_spawn_handle }, sender)
	}

	pub async fn run(&mut self) {
		while let Some(msg) = self.receiver.recv().await {
			log::info!(
				target: &self.eth_client.get_chain_name(),
				"-[{}] 🔖 Received unsigned transaction request: {}",
				sub_display_format(SUB_LOG_TARGET),
				msg.metadata,
			);

			self.spawn_send_transaction(msg).await;
		}
	}

	pub async fn spawn_send_transaction(&self, msg: XtRequestMessage<Call>) {
		let task = UnsignedTransactionTask::new(self.sub_client.clone(), self.eth_client.clone());
		self.xt_spawn_handle.spawn("send_unsigned_transaction", None, async move {
			task.try_send_unsigned_transaction(msg).await
		});
	}
}

pub struct UnsignedTransactionTask<T> {
	sub_client: Arc<OnlineClient<CustomConfig>>,
	eth_client: Arc<EthClient<T>>,
}

impl<T: JsonRpcClient> UnsignedTransactionTask<T> {
	/// Build an Legacy transaction task.
	pub fn new(sub_client: Arc<OnlineClient<CustomConfig>>, eth_client: Arc<EthClient<T>>) -> Self {
		Self { sub_client, eth_client }
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> ExtrinsicTask<T> for UnsignedTransactionTask<T> {
	fn get_eth_client(&self) -> Arc<EthClient<T>> {
		self.eth_client.clone()
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
