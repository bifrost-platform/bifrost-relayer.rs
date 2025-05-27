use alloy::{
	network::AnyNetwork,
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
pub struct EventManager<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<F, P>>,
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

impl<F, P> EventManager<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// Instantiates a new `EventManager` instance.
	pub fn new(
		client: Arc<EthClient<F, P>>,
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
		self.initialize().await?;

		let mut stream = IntervalStream::new(interval(Duration::from_millis(
			self.client.metadata.call_interval,
		)));

		while (stream.next().await).is_some() {
			let latest_block = self.client.get_block_number().await?;
			while self.is_block_confirmed(latest_block) {
				self.process_confirmed_block().await?;

				if self.is_balance_sync_enabled {
					self.client.sync_balance().await?;
				}
			}
		}

		Ok(())
	}

	/// Process the confirmed block and verifies if any events emitted from the target
	/// contracts.
	async fn process_confirmed_block(&mut self) -> Result<()> {
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

	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block: u64) -> bool {
		if self.waiting_block > latest_block {
			return false;
		}
		latest_block.saturating_sub(self.waiting_block) >= self.client.metadata.block_confirmations
	}

	/// Bootstrap phase 0-1.
	async fn wait_provider_sync(&self) -> Result<()> {
		let mut is_first_check = true;
		loop {
			match self.client.syncing().await? {
				SyncStatus::None => {
					break;
				},
				SyncStatus::Info(status) => {
					if is_first_check {
						let msg = format!(
							"⚙️  Syncing: #{:?}, Highest: #{:?} ({} relayer:{})",
							status.current_block,
							status.highest_block,
							self.client.get_chain_name(),
							self.client.address().await,
						);
						sentry::capture_message(&msg, sentry::Level::Warning);
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
	pub async fn bootstrap_0(&self) -> Result<()> {
		self.wait_provider_sync().await?;
		self.initial_flushing().await?;
		Ok(())
	}
}

#[async_trait::async_trait]
impl<F, P> BootstrapHandler for EventManager<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
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
