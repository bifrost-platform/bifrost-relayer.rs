use alloy::{
	network::AnyNetwork,
	primitives::{BlockNumber, ChainId},
	providers::{fillers::TxFiller, Provider, WalletProvider},
	rpc::types::{Filter, Log, SyncStatus},
	transports::Transport,
};
use eyre::Result;
use std::sync::Arc;
use tokio::{
	sync::broadcast::{self, Receiver, Sender},
	time::{sleep, Duration},
};

use br_primitives::{
	bootstrap::BootstrapSharedData, eth::BootstrapState, utils::sub_display_format,
};

use super::{traits::BootstrapHandler, EthClient};

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
pub struct EventManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<F, P, T>>,
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

impl<F, P, T> EventManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Instantiates a new `EventManager` instance.
	pub fn new(
		client: Arc<EthClient<F, P, T>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		is_balance_sync_enabled: bool,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);
		Self { client, sender, waiting_block: 0u64, bootstrap_shared_data, is_balance_sync_enabled }
	}

	/// Initialize event manager.
	async fn initialize(&mut self) -> Result<()> {
		self.client.verify_chain_id().await?;
		self.client.verify_minimum_balance().await?;

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
		self.initialize().await?;

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block = self.client.get_block_number().await?;
				while self.is_block_confirmed(latest_block) {
					self.process_confirmed_block().await?;

					if self.is_balance_sync_enabled {
						self.client.sync_balance().await?;
					}
				}
			}

			sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
		}
	}

	/// Process the confirmed block and verifies if any events emitted from the target
	/// contracts.
	async fn process_confirmed_block(&mut self) -> Result<()> {
		let from = self.waiting_block;
		let to = from.saturating_add(self.client.metadata.get_logs_batch_size.saturating_sub(1u64));

		let filter = if let Some(bitcoin_socket) = &self.client.protocol_contracts.bitcoin_socket {
			Filter::new()
				.from_block(BlockNumber::from(from))
				.to_block(BlockNumber::from(to))
				.address(vec![
					*self.client.protocol_contracts.socket.address(),
					*bitcoin_socket.address(),
				])
		} else {
			Filter::new()
				.from_block(BlockNumber::from(from))
				.to_block(BlockNumber::from(to))
				.address(*self.client.protocol_contracts.socket.address())
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

	/// Verifies if the connected provider is in block sync mode.
	pub async fn wait_provider_sync(&self) -> Result<()> {
		loop {
			match self.client.syncing().await? {
				SyncStatus::None => {
					for state in
						self.bootstrap_shared_data.bootstrap_states.write().await.iter_mut()
					{
						if *state == BootstrapState::NodeSyncing {
							*state = BootstrapState::BootstrapRoundUpPhase1;
						}
					}
					return Ok(());
				},
				SyncStatus::Info(status) => {
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] ⚙️  Syncing: #{:?}, Highest: #{:?}",
						sub_display_format(SUB_LOG_TARGET),
						status.current_block,
						status.highest_block,
					);
				},
			}

			sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
		}
	}
}

#[async_trait::async_trait]
impl<F, P, T> BootstrapHandler for EventManager<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
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
