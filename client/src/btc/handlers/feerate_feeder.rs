use super::{Event, EventMessage, EventType, Handler, LOG_TARGET};
use crate::eth::EthClient;
use alloy::{
	network::AnyNetwork,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use br_primitives::{
	btc::FeeRateResponse,
	substrate::{CustomConfig, initialize_sub_client},
	utils::sub_display_format,
};
use eyre::Result;
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use subxt::OnlineClient;
use tokio::{
	sync::broadcast::Receiver,
	time::{Duration, sleep},
};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

pub const SUB_LOG_TARGET: &str = "btc-feerate-feeder";

pub struct FeeRateFeeder<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	bfc_client: Arc<EthClient<F, P>>,
	sub_client: Option<OnlineClient<CustomConfig>>,
	event_stream: BroadcastStream<EventMessage>,
	handle: SpawnTaskHandle,
	feerate_api: &'static str,
	debug_mode: bool,
}

impl<F, P> FeeRateFeeder<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	pub fn new(
		bfc_client: Arc<EthClient<F, P>>,
		event_receiver: Receiver<EventMessage>,
		handle: SpawnTaskHandle,
		feerate_api: &'static str,
		debug_mode: bool,
	) -> Self {
		Self {
			bfc_client,
			sub_client: None,
			event_stream: BroadcastStream::new(event_receiver),
			handle,
			feerate_api,
			debug_mode,
		}
	}

	async fn initialize(&mut self) {
		self.sub_client = Some(initialize_sub_client(self.bfc_client.get_url()).await);
	}

	async fn fetch_feerate(&self) -> u64 {
		loop {
			match reqwest::get(self.feerate_api).await {
				Ok(response) => match response.json::<FeeRateResponse>().await {
					Ok(fee_rate) => {
						let fastest = fee_rate.fastest_fee;
						if self.debug_mode {
							log::info!(
								target: LOG_TARGET,
								"-[{}] Fetched fee rate: {:?}",
								sub_display_format(SUB_LOG_TARGET),
								fastest
							);
						}
						break fastest;
					},
					Err(e) => {
						log::warn!(
							target: LOG_TARGET,
							"-[{}] Failed to decode fee rate: {:?}. Retrying...",
							sub_display_format(SUB_LOG_TARGET),
							e
						);
						sleep(Duration::from_secs(5)).await;
					},
				},
				Err(e) => {
					log::warn!(
						target: LOG_TARGET,
						"-[{}] Failed to fetch fee rate: {:?}. Retrying...",
						sub_display_format(SUB_LOG_TARGET),
						e
					);
					sleep(Duration::from_secs(5)).await;
				},
			}
		}
	}

	async fn submit_feerate(&self, feerate: u64) -> Result<()> {
		todo!("implement this")
	}
}

#[async_trait::async_trait]
impl<F, P> Handler for FeeRateFeeder<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	async fn run(&mut self) -> Result<()> {
		self.initialize().await;
		while let Some(Ok(msg)) = self.event_stream.next().await {
			if !self.bfc_client.is_selected_relayer().await?
				|| !self.is_target_event(msg.event_type)
			{
				continue;
			}
			self.submit_feerate(self.fetch_feerate().await).await?;
		}
		Ok(())
	}

	async fn process_event(&self, _event: Event) -> Result<()> {
		unreachable!()
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		matches!(event_type, EventType::NewBlock)
	}
}
