use bitcoincore_rpc::bitcoin::address::NetworkUnchecked;
use bitcoincore_rpc::bitcoin::{Address, Amount, Txid};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
/// The response from the fee rate API. (=mempool.space)
pub struct FeeRateResponse {
	#[serde(rename = "fastestFee")]
	pub fastest_fee: u64,
	#[serde(rename = "economyFee")]
	pub economy_fee: u64,
	#[serde(rename = "minimumFee")]
	pub minimum_fee: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// A Bitcoin related event type.
pub enum EventType {
	/// An inbound action.
	Inbound,
	/// An outbound action.
	Outbound,
	/// A new block has been created.
	NewBlock,
}

#[derive(Debug, Clone)]
/// A Bitcoin related event details. (Only for `Inbound` and `Outbound`)
pub struct Event {
	/// The transaction hash.
	pub txid: Txid,
	/// The output index of the transaction.
	pub index: u32,
	/// The account address.
	pub address: Address<NetworkUnchecked>,
	/// The transferred amount.
	pub amount: Amount,
}

#[derive(Debug, Clone)]
/// The event message delivered through channels.
pub struct EventMessage {
	/// The current block number.
	pub block_number: u64,
	/// The event type.
	pub event_type: EventType,
	/// The event details.
	pub events: Vec<Event>,
}

impl EventMessage {
	/// Instantiates a new `EventMessage` instance.
	pub fn new(block_number: u64, event_type: EventType, events: Vec<Event>) -> Self {
		Self { block_number, event_type, events }
	}

	/// Instantiates an `Inbound` typed `EventMessage` instance.
	pub fn inbound(block_number: u64) -> Self {
		Self::new(block_number, EventType::Inbound, vec![])
	}

	/// Instantiates an `Outbound` typed `EventMessage` instance.
	pub fn outbound(block_number: u64) -> Self {
		Self::new(block_number, EventType::Outbound, vec![])
	}

	pub fn new_block(block_number: u64) -> Self {
		Self::new(block_number, EventType::NewBlock, vec![])
	}
}
