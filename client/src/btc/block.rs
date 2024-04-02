use std::{collections::BTreeSet, str::FromStr, sync::Arc, time::Duration};

use bitcoincore_rpc::{
	bitcoincore_rpc_json::GetRawTransactionResultVout, jsonrpc, Client as BtcClient, Error, RpcApi,
};
use ethers::providers::JsonRpcClient;
use miniscript::bitcoin::{
	address::NetworkUnchecked,
	{Address, Amount, Txid},
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast::{self, Receiver, Sender};
use tokio_stream::StreamExt;

use crate::btc::storage::pending_outbound::PendingOutboundPool;
use br_primitives::{bootstrap::BootstrapSharedData, eth::BootstrapState};

use crate::eth::EthClient;

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

pub struct BlockManager<T> {
	btc_client: BtcClient,
	bfc_client: Arc<EthClient<T>>,
	_pending_outbounds: PendingOutboundPool,
	sender: Sender<EventMessage>,
	block_confirmations: u64,
	waiting_block: u64,
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

const INTERVAL: u64 = 1000;
const RETRY_ATTEMPTS: u8 = 10;

#[async_trait::async_trait]
impl<C: JsonRpcClient> RpcApi for BlockManager<C> {
	async fn call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> bitcoincore_rpc::Result<T> {
		for _ in 0..RETRY_ATTEMPTS {
			match self.btc_client.call(cmd, args).await {
				Ok(ret) => return Ok(ret),
				Err(Error::JsonRpc(jsonrpc::error::Error::Rpc(ref err))) if err.code == -28 => {
					tokio::time::sleep(Duration::from_millis(INTERVAL)).await;
					continue;
				},
				Err(e) => return Err(e),
			}
		}
		self.btc_client.call(cmd, args).await
	}
}

// TODO: Remove failable .unwrap()
impl<T: JsonRpcClient + 'static> BlockManager<T> {
	pub fn new(
		btc_client: BtcClient,
		bfc_client: Arc<EthClient<T>>,
		pending_outbounds: PendingOutboundPool,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Self {
		let (sender, _receiver) = broadcast::channel(512);

		Self {
			btc_client,
			bfc_client,
			_pending_outbounds: pending_outbounds,
			sender,
			block_confirmations: 0,
			waiting_block: 0,
			bootstrap_shared_data,
		}
	}

	pub fn subscribe(&self) -> Receiver<EventMessage> {
		self.sender.subscribe()
	}

	pub async fn run(&mut self) {
		self.waiting_block = self.btc_client.get_block_count().await.unwrap(); // TODO: should set at bootstrap process in production

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let latest_block_num = self.btc_client.get_block_count().await.unwrap();
				let (vault_set, refund_set) = self.fetch_registration_sets().await;
				while self.is_block_confirmed(latest_block_num) {
					self.process_confirmed_block(latest_block_num, &vault_set, &refund_set).await;
				}
			}

			self.btc_client.wait_for_new_block(0).await.unwrap();
		}
	}

	#[inline]
	async fn fetch_registration_sets(
		&self,
	) -> (BTreeSet<Address<NetworkUnchecked>>, BTreeSet<Address<NetworkUnchecked>>) {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();
		let vault_set: BTreeSet<Address<NetworkUnchecked>> = registration_pool
			.vault_addresses()
			.await
			.unwrap()
			.iter()
			.map(|s| Address::from_str(s).unwrap())
			.collect();
		let refund_set: BTreeSet<Address<NetworkUnchecked>> = registration_pool
			.refund_addresses()
			.await
			.unwrap()
			.iter()
			.map(|s| Address::from_str(s).unwrap())
			.collect();

		(vault_set, refund_set)
	}

	/// Verifies if the stored waiting block has waited enough.
	#[inline]
	fn is_block_confirmed(&self, latest_block_num: u64) -> bool {
		latest_block_num.saturating_sub(self.waiting_block) >= self.block_confirmations // TODO: pending block's conf:0, latest block's conf:1. something weird
	}

	#[inline]
	async fn process_confirmed_block(
		&mut self,
		to_block: u64,
		vault_set: &BTreeSet<Address<NetworkUnchecked>>,
		refund_set: &BTreeSet<Address<NetworkUnchecked>>,
	) {
		let from_block = self.waiting_block;

		for num in from_block..=to_block {
			let (mut inbound, mut outbound) =
				(EventMessage::inbound(num, vec![]), EventMessage::outbound(num, vec![]));

			let block_hash = self.btc_client.get_block_hash(num).await.unwrap();
			let txs = self.btc_client.get_block_info_with_txs(&block_hash).await.unwrap().tx;

			let mut stream = tokio_stream::iter(txs.iter());
			while let Some(tx) = stream.next().await {
				self.filter(
					tx.txid,
					&tx.vout,
					&mut inbound.events,
					&mut outbound.events,
					vault_set,
					refund_set,
				)
				.await;
			}

			self.sender.send(inbound).unwrap();
			self.sender.send(outbound).unwrap();
		}

		self.increment_waiting_block(to_block);
	}

	#[inline]
	async fn filter(
		&self,
		txid: Txid,
		vouts: &[GetRawTransactionResultVout],
		inbound_events: &mut Vec<Event>,
		outbound_events: &mut Vec<Event>,
		vault_set: &BTreeSet<Address<NetworkUnchecked>>,
		refund_set: &BTreeSet<Address<NetworkUnchecked>>,
	) {
		let mut stream = tokio_stream::iter(vouts.iter());
		while let Some(vout) = stream.next().await {
			if let Some(address) = vout.script_pub_key.address.clone() {
				if vault_set.contains(&address) {
					inbound_events.push(Event {
						txid,
						index: vout.n,
						address: address.clone(),
						amount: vout.value,
					});
				}
				// TODO: filter is really cccp related txo
				if refund_set.contains(&address) {
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

	#[inline]
	pub async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}
