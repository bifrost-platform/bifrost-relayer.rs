use crate::btc::block::{Event, EventMessage as BTCEventMessage, EventType};
use crate::btc::handlers::{BootstrapHandler, Handler, LOG_TARGET};
use crate::eth::EthClient;
use bitcoincore_rpc::bitcoin::Transaction;
use br_primitives::bootstrap::BootstrapSharedData;
use br_primitives::contracts::socket::RequestID;
use br_primitives::eth::{BootstrapState, SocketEventStatus};
use br_primitives::periodic::RollbackSender;
use br_primitives::sub_display_format;
use br_primitives::tx::TxRequestSender;
use ethers::providers::JsonRpcClient;
use ethers::types::Address as EthAddress;
use miniscript::bitcoin::address::NetworkUnchecked;
use miniscript::bitcoin::{Address as BtcAddress, Amount, Network};
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "Inbound-handler";

pub struct InboundHandler<T> {
	/// `EthClient` for interact with Bifrost network.
	bfc_client: Arc<EthClient<T>>,
	/// Sender that sends messages to tx request channel (Bifrost network)
	tx_request_sender: Arc<TxRequestSender>,
	/// Rollback sender that sends rollbackable socket messages.
	rollback_sender: Arc<RollbackSender>,
	/// The receiver that consumes new events from the block channel.
	event_receiver: Receiver<BTCEventMessage>,
	/// Event type which this handler should handle.
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// Bitcoin network (Bitcoin | Testnet | Regtest)
	network: Network,
}

impl<T: JsonRpcClient> InboundHandler<T> {
	fn new(
		bfc_client: Arc<EthClient<T>>,
		tx_request_sender: Arc<TxRequestSender>,
		rollback_sender: Arc<RollbackSender>,
		event_receiver: Receiver<BTCEventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		network: Network,
	) -> Self {
		Self {
			bfc_client,
			tx_request_sender,
			rollback_sender,
			event_receiver,
			target_event: EventType::Inbound,
			bootstrap_shared_data,
			network,
		}
	}

	async fn is_selected_relayer(&self) -> bool {
		let relayer_manager = self.bfc_client.protocol_contracts.relayer_manager.as_ref().unwrap();

		let round = self
			.bfc_client
			.contract_call(relayer_manager.latest_round(), "relayer_manager.latest_round")
			.await;
		self.bfc_client
			.contract_call(
				relayer_manager.is_previous_selected_relayer(
					round,
					self.bfc_client.address(),
					false,
				),
				"relayer_manager.is_previous_selected_relayer",
			)
			.await
	}

	async fn is_sequence_ended(&self, rid: &RequestID) -> bool {
		let request = self
			.bfc_client
			.contract_call(
				self.bfc_client.protocol_contracts.socket.get_request(rid.clone()),
				"socket.get_request",
			)
			.await;
		matches!(
			SocketEventStatus::from(&request.field[0]),
			SocketEventStatus::Committed | SocketEventStatus::Rollbacked
		)
	}

	async fn get_user_bfc_address(
		&self,
		vault_address: &BtcAddress<NetworkUnchecked>,
	) -> Option<EthAddress> {
		todo!()
	}

	async fn send_socket_message(
		&self,
		vault_address: BtcAddress<NetworkUnchecked>,
		amount: Amount,
	) {
		if let Some(user_bfc_address) = self.get_user_bfc_address(&vault_address).await {
			todo!("Open CCCP vote")
		} else {
			todo!("Unmapped vault address? -> is it possible?")
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Handler for InboundHandler<T> {
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

		if !self.is_selected_relayer().await {
			// do nothing if not selected
			return;
		}

		self.send_socket_message(event.address, event.amount).await
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
