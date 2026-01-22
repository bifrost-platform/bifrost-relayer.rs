use alloy::{
	network::Network,
	primitives::{BlockNumber, ChainId},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::{Filter, Log, SyncStatus},
};
use eyre::Result;
use std::sync::Arc;
use tokio::{
	sync::broadcast::{self, Receiver, Sender},
	time::{Duration, interval, sleep},
};
use tokio_stream::{StreamExt, wrappers::IntervalStream};

use br_primitives::{
	bootstrap::BootstrapSharedData, eth::BootstrapState, utils::sub_display_format,
};

use super::{EthClient, traits::BootstrapHandler};

#[derive(Clone, Debug)]
/// The message format passed through the block channel.
pub struct EventMessage {
	/// The processed block number.
	pub block_number: u64,
	/// The detected transaction logs from the target contracts.
	pub event_logs: Vec<Log>,
}

impl EventMessage {
	pub fn new(block_number: u64, event_logs: Vec<Log>) -> Self {
		Self { block_number, event_logs }
	}
}

/// The message receiver connected to the block channel.
pub struct EventReceiver {
	/// The chain ID of the block channel.
	pub id: ChainId,
	/// The message receiver.
	pub receiver: Receiver<EventMessage>,
}

impl EventReceiver {
	pub fn new(id: ChainId, receiver: Receiver<EventMessage>) -> Self {
		Self { id, receiver }
	}
}

const SUB_LOG_TARGET: &str = "event-manager";

/// The essential task that listens and handle new events.
pub struct EventManager<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<F, P, N>>,
	/// The channel sending event messages.
	pub sender: Sender<EventMessage>,
	/// The block waiting for enough confirmations.
	waiting_block: u64,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The flag whether the relayer has enabled self balance synchronization. This field will be
	/// enabled when prometheus exporter is enabled.
	is_balance_sync_enabled: bool,
}

impl<F, P, N: Network> EventManager<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// Instantiates a new `EventManager` instance.
	pub fn new(
		client: Arc<EthClient<F, P, N>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		is_balance_sync_enabled: bool,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);
		Self { client, sender, waiting_block: 0u64, bootstrap_shared_data, is_balance_sync_enabled }
	}

	/// Initialize event manager.
	async fn initialize(&mut self) -> Result<()> {
		self.client.verify_chain_id().await?;

		// initialize waiting block to the latest block + 1
		let latest_block = self.client.get_block_number().await?;
		self.waiting_block = latest_block.saturating_add(1u64);
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] 💤 Idle, best: #{:?}",
			sub_display_format(SUB_LOG_TARGET),
			latest_block
		);

		if self.is_balance_sync_enabled {
			self.client.sync_balance().await?;
		}
		Ok(())
	}

	/// Starts the event manager. Reads every new mined block of the connected chain and starts to
	/// publish to the event channel.
	pub async fn run(&mut self) -> Result<()> {
		self.wait_for_all_chains_bootstrapped().await?;

		let mut stream = IntervalStream::new(interval(Duration::from_millis(
			self.client.metadata.call_interval,
		)));

		while (stream.next().await).is_some() {
			let latest_block = self.client.get_block_number().await?;
			while self.has_new_block(latest_block) {
				self.process_new_block().await?;

				if self.is_balance_sync_enabled {
					self.client.sync_balance().await?;
				}
			}
		}

		Ok(())
	}

	/// Process the new block and verifies if any events emitted from the target contracts.
	/// Note: Events are broadcast immediately without waiting for block confirmations.
	/// Handlers that need confirmation (e.g., SocketRelayHandler) should implement their own waiting logic.
	async fn process_new_block(&mut self) -> Result<()> {
		let from = self.waiting_block;
		let to = from.saturating_add(self.client.metadata.get_logs_batch_size.saturating_sub(1u64));

		let filter = match &self.client.protocol_contracts.bitcoin_socket {
			Some(bitcoin_socket) => Filter::new()
				.from_block(BlockNumber::from(from))
				.to_block(BlockNumber::from(to))
				.address(vec![
					*self.client.protocol_contracts.socket.address(),
					*bitcoin_socket.address(),
				]),
			_ => Filter::new()
				.from_block(BlockNumber::from(from))
				.to_block(BlockNumber::from(to))
				.address(*self.client.protocol_contracts.socket.address()),
		};

		let target_logs = self.client.get_logs(&filter).await?;
		if !target_logs.is_empty() {
			self.sender.send(EventMessage::new(self.waiting_block, target_logs)).unwrap();
		}

		if from < to {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ✨ Imported #({:?} … {:?})",
				sub_display_format(SUB_LOG_TARGET),
				from,
				to
			);
		} else {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ✨ Imported #{:?}",
				sub_display_format(SUB_LOG_TARGET),
				self.waiting_block
			);
		}

		self.increment_waiting_block(to);

		Ok(())
	}

	/// Increment the waiting block.
	fn increment_waiting_block(&mut self, to: u64) {
		self.waiting_block = to.saturating_add(1u64);
		br_metrics::set_block_height(&self.client.get_chain_name(), self.waiting_block);
	}

	/// Verifies if there are new blocks to process.
	#[inline]
	fn has_new_block(&self, latest_block: u64) -> bool {
		if self.waiting_block > latest_block {
			// difference is greater than 100 blocks
			// if it suddenly rollbacked to an old block
			if self.waiting_block > latest_block + 100 {
				br_primitives::log_and_capture_simple!(
					warn,
					"⚠️ [{}] Block rollbacked. From #{:?} to #{:?}",
					self.client.get_chain_name(),
					self.waiting_block,
					latest_block
				);
			}
			return false;
		}
		// Process immediately without waiting for confirmations
		self.waiting_block <= latest_block
	}

	/// Bootstrap phase 0-1.
	async fn wait_provider_sync(&self) -> Result<()> {
		let mut is_first_check = true;
		loop {
			match self.client.syncing().await {
				Ok(SyncStatus::None) => {
					break;
				},
				Ok(SyncStatus::Info(status)) => {
					if is_first_check {
						br_primitives::log_and_capture_simple!(
							warn,
							"⚙️  Syncing: #{:?}, Highest: #{:?} ({} relayer:{})",
							status.current_block,
							status.highest_block,
							self.client.get_chain_name(),
							self.client.address().await
						);
						is_first_check = false;
					}
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚙️  Syncing: #{:?}, Highest: #{:?}",
						sub_display_format(SUB_LOG_TARGET),
						status.current_block,
						status.highest_block,
					);
				},
				Err(_) => {
					break;
				},
			}
			sleep(Duration::from_millis(3000)).await;
		}
		self.set_bootstrap_state(BootstrapState::FlushingStalledTransactions).await;
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] NodeSyncing → FlushingStalledTransactions",
			sub_display_format(SUB_LOG_TARGET),
		);
		Ok(())
	}

	/// Bootstrap phase 0-2.
	async fn initial_flushing(&self) -> Result<()> {
		self.client.flush_stalled_transactions().await?;
		self.set_bootstrap_state(BootstrapState::BootstrapRoundUpPhase1).await;

		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] FlushingStalledTransactions → BootstrapRoundUpPhase1",
			sub_display_format(SUB_LOG_TARGET),
		);
		Ok(())
	}

	/// Bootstrap phase 0-1, 0-2.
	pub async fn bootstrap_0(&mut self) {
		self.initialize().await.unwrap();
		let should_bootstrap = self.is_before_bootstrap_state(BootstrapState::NormalStart).await;
		if should_bootstrap {
			self.wait_provider_sync().await.unwrap();
			self.initial_flushing().await.unwrap();
		}
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> BootstrapHandler for EventManager<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn get_chain_id(&self) -> ChainId {
		self.client.metadata.id
	}

	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<Log>> {
		Ok(vec![])
	}
}
