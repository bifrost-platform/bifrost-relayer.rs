use br_primitives::{
	sub_display_format,
	substrate::CustomConfig,
	tx::{XtRequestMessage, XtRequestMetadata},
};
use ethers::providers::JsonRpcClient;
use std::{error::Error, sync::Arc, time::Duration};
use subxt::{blocks::ExtrinsicEvents, tx::TxPayload, Config, OnlineClient};
use tokio::time::sleep;

use crate::eth::EthClient;

const SUB_LOG_TARGET: &str = "unsigned-tx-manager";

pub struct UnsignedTransactionManager<T> {
	sub_client: Arc<OnlineClient<CustomConfig>>,
	eth_client: Arc<EthClient<T>>,
}

impl<T: JsonRpcClient> UnsignedTransactionManager<T> {
	pub fn new(sub_client: Arc<OnlineClient<CustomConfig>>, eth_client: Arc<EthClient<T>>) -> Self {
		Self { sub_client, eth_client }
	}

	pub async fn try_send_unsigned_transaction<Call: TxPayload>(
		&self,
		msg: XtRequestMessage<Call>,
	) {
		match self.sub_client.tx().create_unsigned(&msg.call) {
			Ok(xt) => match xt.submit_and_watch().await {
				Ok(progress) => match progress.wait_for_finalized_success().await {
					Ok(events) => self.handle_success_tx_request(events, msg.metadata).await,
					Err(error) => self.handle_failed_tx_request(msg, &error).await,
				},
				Err(error) => self.handle_failed_tx_request(msg, &error).await,
			},
			Err(error) => self.handle_failed_tx_creation(msg, &error).await,
		}
	}

	async fn retry_transaction<Call: TxPayload>(&self, mut msg: XtRequestMessage<Call>) {
		msg.build_retry_event();
		sleep(Duration::from_millis(msg.retry_interval)).await;
		self.try_send_unsigned_transaction(msg).await;
	}

	async fn handle_failed_tx_request<Call, E>(&self, msg: XtRequestMessage<Call>, error: &E)
	where
		Call: TxPayload,
		E: Error + Sync + ?Sized,
	{
		log::error!(
			target: &self.eth_client.get_chain_name(),
			"-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(SUB_LOG_TARGET),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		sentry::capture_message(
            format!(
                "[{}]-[{}]-[{}] ♻️  Unknown error while requesting a transaction request: {}, Retries left: {:?}, Error: {}",
                &self.eth_client.get_chain_name(),
                SUB_LOG_TARGET,
                self.eth_client.address(),
                msg.metadata,
                msg.retries_remaining - 1,
                error
            )
                .as_str(),
            sentry::Level::Error,
        );
		self.retry_transaction(msg).await;
	}

	async fn handle_failed_tx_creation<Call, E>(&self, msg: XtRequestMessage<Call>, error: &E)
	where
		Call: TxPayload,
		E: Error + Sync + ?Sized,
	{
		log::error!(
			target: &self.eth_client.get_chain_name(),
			"-[{}] ♻️  Unknown error while creating a tx request: {}, Retries left: {:?}, Error: {}",
			sub_display_format(SUB_LOG_TARGET),
			msg.metadata,
			msg.retries_remaining - 1,
			error.to_string(),
		);
		sentry::capture_message(
            format!(
                "[{}]-[{}]-[{}] ♻️  Unknown error while creating a transaction request: {}, Retries left: {:?}, Error: {}",
                &self.eth_client.get_chain_name(),
                SUB_LOG_TARGET,
                self.eth_client.address(),
                msg.metadata,
                msg.retries_remaining - 1,
                error
            )
                .as_str(),
            sentry::Level::Error,
        );
		self.retry_transaction(msg).await;
	}

	async fn handle_success_tx_request<C>(
		&self,
		events: ExtrinsicEvents<C>,
		metadata: XtRequestMetadata,
	) where
		C: Config,
	{
		todo!()
	}
}
