use crate::btc::storage::vault_set::VaultAddressSet;
use bitcoincore_rpc::bitcoincore_rpc_json::GetRawTransactionResultVout;
use bitcoincore_rpc::{jsonrpc, Client, Error, RpcApi};
use br_primitives::bootstrap::BootstrapSharedData;
use br_primitives::eth::BootstrapState;
use miniscript::bitcoin::address::NetworkUnchecked;
use miniscript::bitcoin::{Address, Amount, Txid};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::broadcast::{Receiver, Sender};
use tokio_stream::StreamExt;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum EventType {
	Inbound,
	Outbound,
}

#[derive(Debug, Clone)]
pub struct Event {
	pub txid: Txid,
	pub index: u32,
	pub address: Address<NetworkUnchecked>,
	pub amount: Amount,
}

#[derive(Debug, Clone)]
pub struct EventMessage {
	pub block_number: u64,
	pub event_type: EventType,
	pub events: Vec<Event>,
}

impl EventMessage {
	pub fn new(block_number: u64, event_type: EventType, events: Vec<Event>) -> Self {
		Self { block_number, event_type, events }
	}

	pub fn inbound(block_number: u64, events: Vec<Event>) -> Self {
		Self::new(block_number, EventType::Inbound, events)
	}
	pub fn outbound(block_number: u64, events: Vec<Event>) -> Self {
		Self::new(block_number, EventType::Outbound, events)
	}
}

pub struct BlockManager {
	client: Client,
	sender: Sender<EventMessage>,
	block_confirmations: u64,
	waiting_block: u64,
	vault_set: VaultAddressSet,
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

const INTERVAL: u64 = 1000;
const RETRY_ATTEMPTS: u8 = 10;

#[async_trait::async_trait]
impl RpcApi for BlockManager {
	async fn call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> bitcoincore_rpc::Result<T> {
		for _ in 0..RETRY_ATTEMPTS {
			match self.client.call(cmd, args).await {
				Ok(ret) => return Ok(ret),
				Err(Error::JsonRpc(jsonrpc::error::Error::Rpc(ref err))) if err.code == -28 => {
					tokio::time::sleep(Duration::from_millis(INTERVAL)).await;
					continue;
				},
				Err(e) => return Err(e),
			}
		}
		self.client.call(cmd, args).await
	}
}

// TODO: Remove failable .unwrap()
impl BlockManager {
	pub fn new(
		client: Client,
		vault_set: VaultAddressSet,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self {
			client,
			sender,
			block_confirmations: 0,
			waiting_block: 0,
			vault_set,
			bootstrap_shared_data,
		}
	}

	pub fn subscribe(&self) -> Receiver<EventMessage> {
		self.sender.subscribe()
	}

	pub async fn run(&mut self) {
		self.waiting_block = self.client.get_block_count().await.unwrap(); // TODO: should set at bootstrap process in production

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block_num = self.client.get_block_count().await.unwrap();
				while self.is_block_confirmed(latest_block_num) {
					self.process_confirmed_block(latest_block_num).await;
				}
			}

			tokio::time::sleep(Duration::from_secs(60)).await; // TODO: interval
		}
	}

	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block_num: u64) -> bool {
		latest_block_num.saturating_sub(self.waiting_block) >= self.block_confirmations // TODO: pending block's conf:0, latest block's conf:1. something weird
	}

	async fn process_confirmed_block(&mut self, to_block: u64) {
		let from_block = self.waiting_block;

		for num in from_block..=to_block {
			let (mut inbound, mut outbound) =
				(EventMessage::inbound(num, vec![]), EventMessage::outbound(num, vec![]));

			let block_hash = self.client.get_block_hash(num).await.unwrap();
			let txs = self.client.get_block_info_with_txs(&block_hash).await.unwrap().tx;

			let mut stream = tokio_stream::iter(txs.iter());
			while let Some(tx) = stream.next().await {
				self.filter(tx.txid, &tx.vout, &mut inbound.events, &mut outbound.events).await;
			}

			self.sender.send(inbound).unwrap();
			self.sender.send(outbound).unwrap();
		}

		self.increment_waiting_block(to_block);
	}

	async fn filter(
		&self,
		txid: Txid,
		vouts: &[GetRawTransactionResultVout],
		inbound_events: &mut Vec<Event>,
		outbound_events: &mut Vec<Event>,
	) {
		let mut stream = tokio_stream::iter(vouts.iter());
		while let Some(vout) = stream.next().await {
			if let Some(address) = vout.script_pub_key.address.clone() {
				if self.vault_set.contains(&address).await {
					inbound_events.push(Event {
						txid,
						index: vout.n,
						address: address.clone(),
						amount: vout.value,
					});
				}
				if let Some(_) = self.pending_outbounds.get(&address, vout.value).await {
					outbound_events.push(Event {
						txid,
						index: vout.n,
						address,
						amount: vout.value,
					});
				}
			}
		}
	}

	#[inline]
	fn increment_waiting_block(&mut self, to: u64) {
		self.waiting_block = to.saturating_add(1);
	}

	pub async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}
