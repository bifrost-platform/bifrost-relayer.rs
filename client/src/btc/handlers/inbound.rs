use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::EthClient,
};

use bitcoincore_rpc::bitcoin::Txid;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::bitcoin_socket::BitcoinSocketContract,
	eth::BootstrapState,
	substrate::CustomConfig,
	tx::{BitcoinRelayMetadata, TxRequestMetadata, TxRequestSender},
	utils::sub_display_format,
};

use super::{BootstrapHandler, EventMessage, TxRequester};
use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{Address as EthAddress, Address, TransactionRequest},
};
use miniscript::bitcoin::{address::NetworkUnchecked, hashes::Hash, Address as BtcAddress};
use sp_core::H256;
use std::sync::Arc;
use subxt::tx::Signer;
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "inbound-handler";

pub struct InboundHandler<T, S> {
	/// `EthClient` for interact with Bifrost network.
	bfc_client: Arc<EthClient<T, S>>,
	/// Sender that sends messages to tx request channel (Bifrost network)
	tx_request_sender: Arc<TxRequestSender>,
	/// The receiver that consumes new events from the block channel.
	event_receiver: Receiver<BTCEventMessage>,
	/// Event type which this handler should handle.
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

impl<T, S> InboundHandler<T, S>
where
	T: JsonRpcClient + 'static,
	S: Signer<CustomConfig>,
{
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
			target_event: EventType::Inbound,
			bootstrap_shared_data,
		}
	}

	async fn get_user_bfc_address(
		&self,
		vault_address: &BtcAddress<NetworkUnchecked>,
	) -> Option<EthAddress> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();
		let user_address: EthAddress = self
			.bfc_client
			.contract_call(
				registration_pool.user_address(
					vault_address.clone().assume_checked().to_string(),
					self.get_current_round().await,
				),
				"registration_pool.user_address",
			)
			.await;

		if user_address == EthAddress::zero() {
			None
		} else {
			user_address.into()
		}
	}

	async fn is_rollback_output(&self, txid: Txid, index: u32) -> bool {
		let socket_queue = self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap();

		let slice: [u8; 32] = txid.to_byte_array();

		let psbt_txid = self
			.bfc_client
			.contract_call(
				socket_queue.rollback_output(slice, index.into()),
				"socket_queue.rollback_output",
			)
			.await;

		!H256::from(psbt_txid).is_zero()
	}

	fn build_transaction(&self, event: &Event, user_bfc_address: Address) -> TransactionRequest {
		let calldata = self
			.bitcoin_socket()
			.poll(
				event.txid.to_byte_array(),
				event.index.into(),
				user_bfc_address,
				event.amount.to_sat().into(),
			)
			.calldata()
			.unwrap();

		TransactionRequest::default().data(calldata).to(self.bitcoin_socket().address())
	}

	/// Checks if the relayer has already voted on the event.
	async fn is_relayer_voted(&self, event: &Event, user_bfc_address: Address) -> bool {
		let hash_key = self
			.bfc_client
			.contract_call(
				self.bitcoin_socket().get_hash_key(
					event.txid.to_byte_array(),
					event.index.into(),
					user_bfc_address,
					event.amount.to_sat().into(),
				),
				"bitcoin_socket.get_hash_key",
			)
			.await;

		self.bfc_client
			.contract_call(
				self.bitcoin_socket().is_relayer_voted(hash_key, self.bfc_client.address()),
				"bitcoin_socket.is_relayer_voted",
			)
			.await
	}

	async fn get_current_round(&self) -> u32 {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();
		self.bfc_client
			.contract_call(registration_pool.current_round(), "registration_pool.current_round")
			.await
	}

	#[inline]
	fn bitcoin_socket(&self) -> &BitcoinSocketContract<Provider<T>> {
		self.bfc_client.protocol_contracts.bitcoin_socket.as_ref().unwrap()
	}
}

#[async_trait::async_trait]
impl<T, S> TxRequester<T, S> for InboundHandler<T, S>
where
	T: JsonRpcClient + 'static,
	S: Signer<CustomConfig>,
{
	fn tx_request_sender(&self) -> Arc<TxRequestSender> {
		self.tx_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<T, S>> {
		self.bfc_client.clone()
	}
}

#[async_trait::async_trait]
impl<T, S> Handler for InboundHandler<T, S>
where
	T: JsonRpcClient + 'static,
	S: Signer<CustomConfig> + Send + Sync,
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

				let mut stream = tokio_stream::iter(msg.events);
				while let Some(event) = stream.next().await {
					self.process_event(event).await;
				}
			}
		}
	}

	async fn process_event(&self, mut event: Event) {
		if let Some(user_bfc_address) = self.get_user_bfc_address(&event.address).await {
			// txid from event is in little endian, convert it to big endian
			event.txid = {
				let mut slice: [u8; 32] = event.txid.to_byte_array();
				slice.reverse();
				Txid::from_slice(&slice).unwrap()
			};

			// check if transaction has been submitted to be rollbacked
			if self.is_rollback_output(event.txid, event.index).await {
				return;
			}
			if self.is_relayer_voted(&event, user_bfc_address).await {
				return;
			}

			let tx_request = self.build_transaction(&event, user_bfc_address);
			let metadata =
				BitcoinRelayMetadata::new(event.address, user_bfc_address, event.txid, event.index);
			self.request_send_transaction(
				tx_request,
				TxRequestMetadata::BitcoinSocketRelay(metadata),
				SUB_LOG_TARGET,
			)
			.await;
		}
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}

#[async_trait::async_trait]
impl<T, S> BootstrapHandler for InboundHandler<T, S>
where
	T: JsonRpcClient,
	S: Signer<CustomConfig> + Send + Sync,
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
