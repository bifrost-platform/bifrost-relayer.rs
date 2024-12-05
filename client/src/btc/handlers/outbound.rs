use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::{traits::SocketRelayBuilder, EthClient},
};

use alloy::{
	network::Ethereum,
	primitives::ChainId,
	providers::{
		fillers::{FillProvider, TxFiller},
		Provider, WalletProvider,
	},
	rpc::types::TransactionRequest,
	sol_types::SolEvent,
	transports::Transport,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::{
		socket::{SocketContract::Socket, Socket_Struct::Socket_Message},
		socket_queue::SocketQueueContract::SocketQueueContractInstance,
	},
	eth::{BootstrapState, BuiltRelayTransaction, SocketEventStatus},
	tx::{SocketRelayMetadata, TxRequestMetadata, TxRequestSender},
	utils::sub_display_format,
};
use byteorder::{BigEndian, ByteOrder};
use eyre::Result;
use miniscript::bitcoin::hashes::Hash;
use miniscript::bitcoin::Txid;
use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};
use tokio::sync::broadcast::Receiver;

use super::{BootstrapHandler, EventMessage, TxRequester};

const SUB_LOG_TARGET: &str = "outbound-handler";

pub struct OutboundHandler<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	bfc_client: Arc<EthClient<F, P, T>>,
	clients: Arc<BTreeMap<ChainId, Arc<EthClient<F, P, T>>>>,
	event_receiver: Receiver<BTCEventMessage>,
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
}

impl<F, P, T> OutboundHandler<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	pub fn new(
		bfc_client: Arc<EthClient<F, P, T>>,
		clients: Arc<BTreeMap<ChainId, Arc<EthClient<F, P, T>>>>,
		event_receiver: Receiver<BTCEventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Self {
		Self {
			bfc_client,
			clients,
			event_receiver,
			target_event: EventType::Outbound,
			bootstrap_shared_data,
		}
	}

	#[inline]
	fn socket_queue(
		&self,
	) -> &SocketQueueContractInstance<T, Arc<FillProvider<F, P, T, Ethereum>>> {
		self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap()
	}

	/// Check if the transaction was originated by CCCP. If true, returns the composed socket messages.
	async fn check_socket_queue(&self, txid: Txid) -> Result<Vec<Socket_Message>> {
		let mut slice: [u8; 32] = txid.to_byte_array();
		slice.reverse();

		let socket_messages = self.socket_queue().outbound_tx(slice.into()).call().await?._0;

		socket_messages
			.iter()
			.map(|bytes| Socket::abi_decode_data(bytes, true).map(|decoded| decoded.0))
			.collect::<Result<Vec<_>, _>>()
			.map_err(|e| e.into())
	}
}

// #[async_trait::async_trait]
// impl<F, P, T> TxRequester<F, P, T> for OutboundHandler<F, P, T>
// where
// 	F: TxFiller<Ethereum> + WalletProvider<Ethereum>,
// 	P: Provider<T, Ethereum>,
// 	T: Transport + Clone,
// {
// 	fn tx_request_sender(&self) -> Arc<TxRequestSender> {
// 		self.tx_request_sender.clone()
// 	}

// 	fn bfc_client(&self) -> Arc<EthClient<F, P, T>> {
// 		self.bfc_client.clone()
// 	}
// }

#[async_trait::async_trait]
impl<F, P, T> Handler for OutboundHandler<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	async fn run(&mut self) -> Result<()> {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let msg = self.event_receiver.recv().await.unwrap();

				if !self.bfc_client.is_selected_relayer().await?
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
					let socket_messages = self.check_socket_queue(txid).await?;
					for mut msg in socket_messages {
						msg.status = SocketEventStatus::Executed.into();

						if let Some(built_transaction) =
							self.build_transaction(msg.clone(), false, Default::default()).await?
						{
							// self.request_send_transaction(
							// 	built_transaction.tx_request,
							// 	TxRequestMetadata::SocketRelay(SocketRelayMetadata::new(
							// 		false,
							// 		SocketEventStatus::from(msg.status),
							// 		msg.req_id.sequence,
							// 		BigEndian::read_u32(&msg.req_id.ChainIndex.0) as ChainId,
							// 		BigEndian::read_u32(&msg.ins_code.ChainIndex.0) as ChainId,
							// 		msg.params.to,
							// 		false,
							// 	)),
							// 	SUB_LOG_TARGET,
							// )
							// .await;
							todo!()
						}
					}
				}
			}
		}
	}

	async fn process_event(&self, _event_tx: Event) -> Result<()> {
		unreachable!("unimplemented")
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}

#[async_trait::async_trait]
impl<F, P, T> SocketRelayBuilder<F, P, T> for OutboundHandler<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	fn get_client(&self) -> Arc<EthClient<F, P, T>> {
		self.bfc_client.clone()
	}

	async fn build_transaction(
		&self,
		msg: Socket_Message,
		_: bool,
		_: ChainId,
	) -> Result<Option<BuiltRelayTransaction>> {
		// the original msg must be used for building calldata
		let (signatures, is_external) = self.build_outbound_signatures(msg.clone()).await?;
		return Ok(Some(BuiltRelayTransaction::new(
			TransactionRequest::default()
				.input(self.build_poll_call_data(msg, signatures))
				.to(self.bfc_client.protocol_contracts.socket.address().clone()),
			is_external,
		)));
	}
}

#[async_trait::async_trait]
impl<F, P, T> BootstrapHandler for OutboundHandler<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		unreachable!("unimplemented")
	}

	async fn get_bootstrap_events(&self) -> Result<(EventMessage, EventMessage)> {
		unreachable!("unimplemented")
	}
}
