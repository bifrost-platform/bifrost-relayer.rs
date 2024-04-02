use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{BootstrapHandler, Handler, LOG_TARGET},
	},
	eth::EthClient,
};
use bitcoincore_rpc::bitcoin::Transaction;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	eth::{BootstrapState, GasCoefficient},
	sub_display_format,
	tx::{
		BitcoinSocketRelayMetadata, TxRequest, TxRequestMessage, TxRequestMetadata, TxRequestSender,
	},
};
use ethers::{
	providers::JsonRpcClient,
	types::{Address as EthAddress, Address, TransactionRequest},
};
use miniscript::bitcoin::{address::NetworkUnchecked, hashes::Hash, Address as BtcAddress};
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "Inbound-handler";

pub struct InboundHandler<T> {
	/// `EthClient` for interact with Bifrost network.
	bfc_client: Arc<EthClient<T>>,
	/// Sender that sends messages to tx request channel (Bifrost network)
	tx_request_sender: Arc<TxRequestSender>,
	/// The receiver that consumes new events from the block channel.
	event_receiver: Receiver<BTCEventMessage>,
	/// Event type which this handler should handle.
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

impl<T: JsonRpcClient + 'static> InboundHandler<T> {
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
				registration_pool
					.user_address(vault_address.clone().assume_checked().to_string(), true),
				"registration_pool.user_address",
			)
			.await;

		if user_address == EthAddress::zero() {
			None
		} else {
			user_address.into()
		}
	}

	fn build_transaction(&self, event: &Event, user_bfc_address: Address) -> TransactionRequest {
		let bitcoin_socket = self.bfc_client.protocol_contracts.bitcoin_socket.as_ref().unwrap();
		let calldata = bitcoin_socket
			.poll(
				event.txid.to_byte_array(),
				event.index.into(),
				user_bfc_address,
				event.amount.to_sat().into(),
			)
			.calldata()
			.unwrap();

		TransactionRequest::default().data(calldata).to(bitcoin_socket.address())
	}

	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: BitcoinSocketRelayMetadata,
	) {
		match self.tx_request_sender.send(TxRequestMessage::new(
			TxRequest::Legacy(tx_request),
			TxRequestMetadata::BitcoinSocketRelay(metadata.clone()),
			true,
			false,
			GasCoefficient::Mid,
			false,
		)) {
			Ok(_) => log::info!(
				target: LOG_TARGET,
				"-[{}] 🔖 Request relay transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				log::error!(
					target: LOG_TARGET,
					"-[{}] ❗️ Failed to send relay transaction: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to send relay transaction: {}, Error: {}",
						LOG_TARGET,
						SUB_LOG_TARGET,
						self.bfc_client.address(),
						metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static> Handler for InboundHandler<T> {
	async fn run(&mut self) {
		loop {
			// TODO: BootstrapState::BootstrapBitcoinInbound

			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let msg = self.event_receiver.recv().await.unwrap();

				if !self.is_target_event(msg.event_type) {
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
					self.process_event(event, false).await;
				}
			}
		}
	}

	async fn process_event(&self, event: Event, is_bootstrap: bool) {
		// TODO: if is_bootstrap

		if !self.bfc_client.is_selected_relayer().await {
			// do nothing if not selected
			return;
		}

		if let Some(user_bfc_address) = self.get_user_bfc_address(&event.address).await {
			let tx_request = self.build_transaction(&event, user_bfc_address.clone());
			let metadata = BitcoinSocketRelayMetadata::new(
				event.address,
				user_bfc_address,
				event.txid,
				event.index,
			);
			self.request_send_transaction(tx_request, metadata).await;
		} else {
			todo!("Unmapped vault address? -> erroneous deposit or something")
		}
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for InboundHandler<T> {
	async fn bootstrap(&self) {
		todo!()
	}

	async fn get_bootstrap_events(&self) -> Vec<Transaction> {
		todo!()
	}

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}
