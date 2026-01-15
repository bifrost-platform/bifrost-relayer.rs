use super::{Event, EventMessage, EventType, Handler, LOG_TARGET, XtRequester};
use crate::eth::EthClient;
use alloy::{
	network::Network,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use bitcoincore_rpc::{Client as BtcClient, RpcApi};
use br_primitives::{
	btc::FeeRateResponse,
	constants::{btc::MEMPOOL_SPACE_FEE_RATE_MULTIPLIER, tx::DEFAULT_CALL_RETRY_INTERVAL_MS},
	substrate::{
		CustomConfig, EthereumSignature, FeeRateSubmission, bifrost_runtime, initialize_sub_client,
	},
	tx::{SubmitFeeRateMetadata, XtRequest, XtRequestMessage, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};
use eyre::Result;
use std::sync::Arc;
use subxt::{OnlineClient, ext::subxt_core::utils::AccountId20};
use tokio::{
	sync::broadcast::Receiver,
	time::{Duration, sleep},
};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

pub const SUB_LOG_TARGET: &str = "btc-feerate-feeder";

pub struct FeeRateFeeder<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// `EthClient` to interact with Bifrost network.
	pub bfc_client: Arc<EthClient<F, P, N>>,
	/// `Client` to interact with bitcoin core RPC.
	btc_client: BtcClient,
	/// The substrate client.
	sub_client: Option<OnlineClient<CustomConfig>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The receiver that consumes new events from the block channel.
	event_stream: BroadcastStream<EventMessage>,
	/// The API endpoint for fetching Bitcoin fee rate.
	fee_rate_api: Option<&'static str>,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

impl<F, P, N: Network> FeeRateFeeder<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub fn new(
		bfc_client: Arc<EthClient<F, P, N>>,
		btc_client: BtcClient,
		xt_request_sender: Arc<XtRequestSender>,
		event_receiver: Receiver<EventMessage>,
		fee_rate_api: Option<&'static str>,
		debug_mode: bool,
	) -> Self {
		Self {
			bfc_client,
			btc_client,
			sub_client: None,
			xt_request_sender,
			event_stream: BroadcastStream::new(event_receiver),
			fee_rate_api,
			debug_mode,
		}
	}

	async fn initialize(&mut self) {
		self.sub_client = Some(initialize_sub_client(self.bfc_client.get_url()).await);
	}

	async fn fetch_fee_rate(&self) -> (u64, u64) {
		loop {
			if let Some(fee_rate_api) = self.fee_rate_api {
				match reqwest::get(fee_rate_api).await {
					Ok(response) => match response.json::<FeeRateResponse>().await {
						Ok(fee_rate) => {
							let lt_fee_rate = fee_rate.economy_fee;
							let final_fee_rate = (fee_rate.fastest_fee as f64
								* MEMPOOL_SPACE_FEE_RATE_MULTIPLIER)
								.round() as u64;
							if self.debug_mode {
								log::info!(
									target: LOG_TARGET,
									"-[{}] Fetched fee rate: ({:?}, {:?})",
									sub_display_format(SUB_LOG_TARGET),
									lt_fee_rate,
									final_fee_rate
								);
							}
							break (lt_fee_rate, final_fee_rate);
						},
						Err(e) => match self.fallback_estimate_fee().await {
							Ok(fallback) => {
								log::info!(
									target: LOG_TARGET,
									"-[{}] Using fallback fee rate after decode error: {:?}",
									sub_display_format(SUB_LOG_TARGET),
									e
								);
								break fallback;
							},
							Err(fallback_err) => {
								log::warn!(
									target: LOG_TARGET,
									"-[{}] Failed to decode fee rate: {:?}, fallback also failed: {:?}. Retrying in 5 seconds...",
									sub_display_format(SUB_LOG_TARGET),
									e,
									fallback_err
								);
								sleep(Duration::from_secs(5)).await;
							},
						},
					},
					Err(e) => match self.fallback_estimate_fee().await {
						Ok(fallback) => {
							log::info!(
								target: LOG_TARGET,
								"-[{}] Using fallback fee rate after fetch error: {:?}",
								sub_display_format(SUB_LOG_TARGET),
								e
							);
							break fallback;
						},
						Err(fallback_err) => {
							log::warn!(
								target: LOG_TARGET,
								"-[{}] Failed to fetch fee rate: {:?}, fallback also failed: {:?}. Retrying in 5 seconds...",
								sub_display_format(SUB_LOG_TARGET),
								e,
								fallback_err
							);
							sleep(Duration::from_secs(5)).await;
						},
					},
				}
			} else {
				// we submit a fixed fee rate for regtest
				break (1, 1);
			}
		}
	}

	/// Check if the fee rate has been submitted by this authority.
	async fn is_fee_rate_submitted(&self) -> bool {
		loop {
			match self.sub_client.as_ref().unwrap().storage().at_latest().await {
				Ok(storage) => {
					match storage.fetch(&bifrost_runtime::storage().blaze().fee_rates()).await {
						Ok(Some(fee_rates)) => {
							let address = self.bfc_client.address().await.0.0;
							if fee_rates.0.iter().any(|(key, _)| key.0 == address) {
								return true;
							}
							return false;
						},
						Ok(None) => {
							unreachable!("The fee rate should always be available.")
						},
						Err(_) => {
							tokio::time::sleep(Duration::from_millis(
								DEFAULT_CALL_RETRY_INTERVAL_MS,
							))
							.await;
							continue;
						},
					}
				},
				Err(_) => {
					tokio::time::sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
					continue;
				},
			}
		}
	}

	/// Fallback method to estimate fee rate if the mempool.space API is not available.
	async fn fallback_estimate_fee(&self) -> Result<(u64, u64)> {
		let st_fee_result = self.btc_client.estimate_smart_fee(1, None).await?;
		let lt_fee_result = self.btc_client.estimate_smart_fee(6, None).await?;

		let st_fee_rate = st_fee_result
			.fee_rate
			.ok_or_else(|| eyre::eyre!("Failed to get short-term fee rate from Bitcoin node"))?
			.to_sat() / 1000;
		let lt_fee_rate = lt_fee_result
			.fee_rate
			.ok_or_else(|| eyre::eyre!("Failed to get long-term fee rate from Bitcoin node"))?
			.to_sat() / 1000;

		if self.debug_mode {
			log::info!(
				target: LOG_TARGET,
				"-[{}] Fallback fee rate: ({:?}, {:?})",
				sub_display_format(SUB_LOG_TARGET),
				lt_fee_rate,
				st_fee_rate
			);
		}

		Ok((lt_fee_rate, st_fee_rate))
	}

	async fn build_payload(
		&self,
		lt_fee_rate: u64,
		fee_rate: u64,
	) -> Result<(FeeRateSubmission<AccountId20, u32>, EthereumSignature)> {
		let deadline = self.bfc_client.get_block_number().await? as u32 + 2;
		let msg = FeeRateSubmission {
			authority_id: AccountId20(self.bfc_client.address().await.0.0),
			lt_fee_rate,
			fee_rate,
			deadline,
		};
		let signature_msg = format!("{}:{}:{}", deadline, lt_fee_rate, fee_rate);
		let signature = self.bfc_client.sign_message(signature_msg.as_bytes()).await?.into();
		Ok((msg, signature))
	}

	async fn build_unsigned_tx(
		&self,
		lt_fee_rate: u64,
		fee_rate: u64,
	) -> Result<(XtRequest, SubmitFeeRateMetadata)> {
		let (msg, signature) = self.build_payload(lt_fee_rate, fee_rate).await?;
		let metadata = SubmitFeeRateMetadata::new(lt_fee_rate, fee_rate);
		Ok((
			XtRequest::from(bifrost_runtime::tx().blaze().submit_fee_rate(msg, signature)),
			metadata,
		))
	}

	async fn submit_fee_rate(&self, lt_fee_rate: u64, fee_rate: u64) -> Result<()> {
		let (call, metadata) = self.build_unsigned_tx(lt_fee_rate, fee_rate).await?;
		self.request_send_transaction(call, metadata).await;
		Ok(())
	}

	/// Send the transaction request message to the channel.
	async fn request_send_transaction(&self, call: XtRequest, metadata: SubmitFeeRateMetadata) {
		match self
			.xt_request_sender
			.send(XtRequestMessage::new(call, XtRequestMetadata::SubmitFeeRate(metadata.clone())))
		{
			Ok(_) => log::info!(
				target: &self.bfc_client.get_chain_name(),
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				br_primitives::log_and_capture!(
					error,
					&self.bfc_client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to send unsigned transaction: {}, Error: {}",
					metadata,
					error
				);
			},
		}
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> Handler for FeeRateFeeder<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	async fn run(&mut self) -> Result<()> {
		self.initialize().await;
		while let Some(Ok(msg)) = self.event_stream.next().await {
			if !self.bfc_client.is_selected_relayer().await?
				|| !self.is_target_event(msg.event_type)
			{
				continue;
			}
			// submit fee rate if blaze is activated
			if self.bfc_client.blaze_activation().await? && !self.is_fee_rate_submitted().await {
				let (lt_fee_rate, fee_rate) = self.fetch_fee_rate().await;
				self.submit_fee_rate(lt_fee_rate, fee_rate).await?;
			}
		}
		Ok(())
	}

	async fn process_event(&self, _event: &Event) -> Result<()> {
		unreachable!()
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		matches!(event_type, EventType::NewBlock)
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> XtRequester<F, P, N> for FeeRateFeeder<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<F, P, N>> {
		self.bfc_client.clone()
	}
}
