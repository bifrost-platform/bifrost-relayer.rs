use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::{traits::SocketRelayBuilder, EthClient},
};

use super::{BootstrapHandler, EventMessage, TxRequester};
use br_primitives::substrate::CustomConfig;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::{
		socket::{Socket, SocketMessage},
		socket_queue::SocketQueueContract,
	},
	eth::{BootstrapState, BuiltRelayTransaction, ChainID, SocketEventStatus},
	tx::{SocketRelayMetadata, TxRequestMetadata, TxRequestSender},
	utils::sub_display_format,
};
use ethers::{
	abi::AbiDecode,
	prelude::TransactionRequest,
	providers::{JsonRpcClient, Provider},
	types::Bytes,
};
use miniscript::bitcoin::hashes::Hash;
use miniscript::bitcoin::Txid;
use std::{collections::BTreeSet, sync::Arc};
use subxt::tx::Signer;
use tokio::sync::broadcast::Receiver;

const SUB_LOG_TARGET: &str = "outbound-handler";

pub struct OutboundHandler<T, S> {
	bfc_client: Arc<EthClient<T, S>>,
	tx_request_sender: Arc<TxRequestSender>,
	event_receiver: Receiver<BTCEventMessage>,
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig>> OutboundHandler<T, S> {
	pub fn new(
		bfc_client: Arc<EthClient<T, S>>,
		tx_request_sender: Arc<TxRequestSender>,
		event_receiver: Receiver<BTCEventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Self {
		Self {
			bfc_client,
			tx_request_sender,
			event_receiver,
			target_event: EventType::Outbound,
			bootstrap_shared_data,
		}
	}

	#[inline]
	fn socket_queue(&self) -> &SocketQueueContract<Provider<T>> {
		self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap()
	}

	/// Check if the transaction was originated by CCCP. If true, returns the composed socket messages.
	async fn check_socket_queue(&self, txid: Txid) -> Vec<SocketMessage> {
		let mut slice: [u8; 32] = txid.to_byte_array();
		slice.reverse();

		let socket_messages: Vec<Bytes> = self
			.bfc_client
			.contract_call(self.socket_queue().outbound_tx(slice), "socket_queue.outbound_tx")
			.await;

		socket_messages
			.iter()
			.map(|bytes| Socket::decode(&bytes).unwrap().msg)
			.collect()
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig>> TxRequester<T, S>
	for OutboundHandler<T, S>
{
	fn tx_request_sender(&self) -> Arc<TxRequestSender> {
		self.tx_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<T, S>> {
		self.bfc_client.clone()
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig> + 'static> Handler
	for OutboundHandler<T, S>
where
	S: Send,
	S: Sync,
{
	async fn run(&mut self) {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let msg = self.event_receiver.recv().await.unwrap();

				if !self.bfc_client.is_selected_relayer().await
					|| !self.is_target_event(msg.event_type)
				{
					continue;
				}

				log::info!(
					target: LOG_TARGET,
					"-[{}] 📦 Imported #{:?} with target logs({:?})",
					sub_display_format(SUB_LOG_TARGET),
					msg.block_number,
					msg.events.len()
				);

				let txids: BTreeSet<Txid> = msg.events.iter().map(|event| event.txid).collect();
				for txid in txids {
					let socket_messages = self.check_socket_queue(txid).await;
					for mut msg in socket_messages {
						msg.status = SocketEventStatus::Executed.into();

						if let Some(built_transaction) =
							self.build_transaction(msg.clone(), false, Default::default()).await
						{
							self.request_send_transaction(
								built_transaction.tx_request,
								TxRequestMetadata::SocketRelay(SocketRelayMetadata::new(
									false,
									SocketEventStatus::from(msg.status),
									msg.req_id.sequence,
									ChainID::from_be_bytes(msg.req_id.chain),
									ChainID::from_be_bytes(msg.ins_code.chain),
									msg.params.to,
									false,
								)),
								SUB_LOG_TARGET,
							)
							.await;
						}
					}
				}
			}
		}
	}

	async fn process_event(&self, _event_tx: Event) {
		unreachable!("unimplemented")
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig> + 'static> SocketRelayBuilder<T, S>
	for OutboundHandler<T, S>
where
	S: Send,
	S: Sync,
{
	fn get_client(&self) -> Arc<EthClient<T, S>> {
		self.bfc_client.clone()
	}

	async fn build_transaction(
		&self,
		msg: SocketMessage,
		_: bool,
		_: ChainID,
	) -> Option<BuiltRelayTransaction> {
		// the original msg must be used for building calldata
		let (signatures, is_external) = self.build_outbound_signatures(msg.clone()).await;
		Some(BuiltRelayTransaction::new(
			TransactionRequest::default()
				.data(self.build_poll_call_data(msg, signatures))
				.to(self.bfc_client.protocol_contracts.socket.address()),
			is_external,
		))
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient, S: Signer<CustomConfig> + Send + Sync> BootstrapHandler
	for OutboundHandler<T, S>
{
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) {
		unreachable!("unimplemented")
	}

	async fn get_bootstrap_events(&self) -> (EventMessage, EventMessage) {
		unreachable!("unimplemented")
	}
}
