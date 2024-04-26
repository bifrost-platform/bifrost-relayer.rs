use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::{traits::SocketRelayBuilder, EthClient},
};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::{socket::SocketMessage, socket_queue::SocketQueueContract},
	eth::{BootstrapState, BuiltRelayTransaction, ChainID, SocketEventStatus},
	tx::{BitcoinRelayMetadata, TxRequestSender},
	utils::sub_display_format,
};
use ethers::{
	abi::AbiDecode,
	prelude::TransactionRequest,
	providers::{JsonRpcClient, Provider},
	types::{Address as EthAddress, Bytes},
};
use miniscript::bitcoin::{address::NetworkUnchecked, Address as BtcAddress, Amount, Txid};
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use super::{BootstrapHandler, EventMessage, TxRequester};

const SUB_LOG_TARGET: &str = "outbound-handler";

pub struct OutboundHandler<T> {
	bfc_client: Arc<EthClient<T>>,
	tx_request_sender: Arc<TxRequestSender>,
	event_receiver: Receiver<BTCEventMessage>,
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

impl<T: JsonRpcClient> OutboundHandler<T> {
	pub fn new(
		bfc_client: Arc<EthClient<T>>,
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

	async fn check_socket_queue(
		&self,
		txid: Txid,
		user_bfc_address: EthAddress,
		amount: Amount,
		processed: &mut BTreeSet<Bytes>,
	) -> (bool, SocketMessage) {
		let slice: &[u8; 32] = txid.as_ref();
		let socket_messages: Vec<Bytes> = self
			.bfc_client
			.contract_call(self.socket_queue().outbound_tx(*slice), "socket_queue.outbound_tx")
			.await;

		if socket_messages.is_empty() {
			(false, SocketMessage::default())
		} else {
			for socket_msg_bytes in socket_messages {
				if processed.contains(&socket_msg_bytes) {
					continue;
				}
				let socket_msg: SocketMessage = SocketMessage::decode(&socket_msg_bytes).unwrap();
				if socket_msg.params.to == user_bfc_address
					&& socket_msg.params.amount == amount.to_sat().into()
				{
					processed.insert(socket_msg_bytes);
					return (true, socket_msg);
				}
			}

			(false, SocketMessage::default())
		}
	}

	async fn get_user_bfc_address(
		&self,
		refund_address: &BtcAddress<NetworkUnchecked>,
	) -> Option<EthAddress> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();
		let user_address: EthAddress = self
			.bfc_client
			.contract_call(
				registration_pool
					.user_address(refund_address.clone().assume_checked().to_string(), false),
				"registration_pool.user_address",
			)
			.await;

		if user_address == EthAddress::zero() {
			None
		} else {
			user_address.into()
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> TxRequester<T> for OutboundHandler<T> {
	fn tx_request_sender(&self) -> Arc<TxRequestSender> {
		self.tx_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<T>> {
		self.bfc_client.clone()
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static> Handler for OutboundHandler<T> {
	async fn run(&mut self) {
		loop {
			// TODO: BootstrapState::BootstrapBitcoinOutbound

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

				let mut processed = BTreeSet::new();
				let mut stream = tokio_stream::iter(msg.events);
				while let Some(event) = stream.next().await {
					self.process_event(event, &mut processed, false).await;
				}
			}
		}
	}

	async fn process_event(
		&self,
		event_tx: Event,
		processed: &mut BTreeSet<Bytes>,
		_is_bootstrap: bool,
	) {
		// TODO: if is_bootstrap

		if let Some(user_bfc_address) = self.get_user_bfc_address(&event_tx.address).await {
			let (is_cccp, mut socket_msg) = self
				.check_socket_queue(event_tx.txid, user_bfc_address, event_tx.amount, processed)
				.await;
			if is_cccp {
				socket_msg.status = SocketEventStatus::Executed.into();

				if let Some(built_transaction) = self.build_transaction(socket_msg, false, 0).await
				{
					self.request_send_transaction(
						built_transaction.tx_request,
						BitcoinRelayMetadata::new(
							event_tx.address,
							user_bfc_address,
							event_tx.txid,
							event_tx.index,
						),
						SUB_LOG_TARGET,
					)
					.await;
				}
			}
		}
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static> SocketRelayBuilder<T> for OutboundHandler<T> {
	fn get_client(&self) -> Arc<EthClient<T>> {
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
		return Some(BuiltRelayTransaction::new(
			TransactionRequest::default()
				.data(self.build_poll_call_data(msg, signatures))
				.to(self.bfc_client.protocol_contracts.socket.address()),
			is_external,
		));
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for OutboundHandler<T> {
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
