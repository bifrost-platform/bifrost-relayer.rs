use crate::eth::{BlockMessage, EthClient, EventSender};
use cccp_primitives::{eth::Contract, socket_bifrost::SocketBifrost};
use ethers::{providers::Provider, types::H160};
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;

/// The essential task that handles `Validator Synchronization Protocol` related events.
pub struct VSPHandler<T> {
	/// The event senders that sends messages to the event channel.
	pub event_senders: Vec<Arc<EventSender>>,
	/// The block receiver that consumes new blocks from the block channel.
	pub block_receiver: Receiver<BlockMessage>,
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The address of the `Socket.Bifrost` contract.
	pub target_contract: H160,
	/// The target `Socket` contract instance.
	pub target_socket: SocketBifrost<Provider<T>>,
	/// The socket contracts supporting CCCP.
	pub socket_contracts: Vec<Contract>,
}
