use super::{BfcClient, CustomConfig};
use br_primitives::bootstrap::BootstrapSharedData;
use ethers::providers::JsonRpcClient;
use ethers::types::U64;
use std::sync::Arc;
use subxt::events::EventDetails;
use tokio::sync::broadcast::{self, Sender};

const SUB_LOG_TARGET: &str = "event-manager";

#[derive(Debug, Clone)]
/// The message format passed through the block channel.
pub struct EventMessage {
	/// The processed block number.
	pub block_number: U64,
	/// The detected transaction logs from the target contracts.
	pub events: Vec<EventDetails<CustomConfig>>,
}

impl EventMessage {
	pub fn new(block_number: U64, events: Vec<EventDetails<CustomConfig>>) -> Self {
		Self { block_number, events }
	}
}

/// The essential task that listens and handle new events.
pub struct EventManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<BfcClient<T>>,
	/// The channel sending event messages.
	pub sender: Sender<EventMessage>,
	/// The block waiting for enough confirmations.
	waiting_block: U64,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The flag whether the relayer has enabled self balance synchronization. This field will be
	/// enabled when prometheus exporter is enabled.
	is_balance_sync_enabled: bool,
}

impl<T: JsonRpcClient> EventManager<T> {
	pub fn new(
		client: Arc<BfcClient<T>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		is_balance_sync_enabled: bool,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self {
			client,
			sender,
			waiting_block: U64::default(),
			bootstrap_shared_data,
			is_balance_sync_enabled,
		}
	}
}
