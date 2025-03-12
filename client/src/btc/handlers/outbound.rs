use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::{EthClient, send_transaction, traits::SocketRelayBuilder},
};

use alloy::{
	network::AnyNetwork,
	primitives::ChainId,
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::TransactionRequest,
	sol_types::SolEvent,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::{
		socket::{Socket_Struct::Socket_Message, SocketContract::Socket},
		socket_queue::SocketQueueInstance,
	},
	eth::{BootstrapState, BuiltRelayTransaction, SocketEventStatus},
	tx::{SocketRelayMetadata, TxRequestMetadata},
	utils::sub_display_format,
};
use eyre::Result;
use miniscript::bitcoin::{Txid, hashes::Hash};
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use super::{BootstrapHandler, EventMessage};

const SUB_LOG_TARGET: &str = "outbound-handler";

pub struct OutboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	pub bfc_client: Arc<EthClient<F, P>>,
	event_stream: BroadcastStream<BTCEventMessage>,
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

impl<F, P> OutboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	pub fn new(
		bfc_client: Arc<EthClient<F, P>>,
		event_receiver: Receiver<BTCEventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
	) -> Self {
		Self {
			bfc_client,
			event_stream: BroadcastStream::new(event_receiver),
			target_event: EventType::Outbound,
			bootstrap_shared_data,
			handle,
			debug_mode,
		}
	}

	#[inline]
	fn socket_queue(&self) -> &SocketQueueInstance<F, P> {
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

#[async_trait::async_trait]
impl<F, P> Handler for OutboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<AnyNetwork> + 'static,
{
	async fn run(&mut self) -> Result<()> {
		self.wait_for_bootstrap_state(BootstrapState::NormalStart).await?;
		while let Some(Ok(msg)) = self.event_stream.next().await {
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

			let mut stream = tokio_stream::iter(msg.events);
			while let Some(event) = stream.next().await {
				self.process_event(event).await?;
			}
		}

		Ok(())
	}

	async fn process_event(&self, event: Event) -> Result<()> {
		let mut stream = tokio_stream::iter(self.check_socket_queue(event.txid).await?.into_iter());
		while let Some(mut msg) = stream.next().await {
			msg.status = SocketEventStatus::Executed.into();

			if let Some(built_transaction) =
				self.build_transaction(msg.clone(), false, Default::default()).await?
			{
				send_transaction(
					self.bfc_client.clone(),
					built_transaction.tx_request,
					format!("{} ({})", SUB_LOG_TARGET, self.bfc_client.get_chain_name()),
					TxRequestMetadata::SocketRelay(SocketRelayMetadata::new(
						false,
						SocketEventStatus::from(msg.status),
						msg.req_id.sequence,
						Into::<u32>::into(msg.req_id.ChainIndex) as ChainId,
						Into::<u32>::into(msg.ins_code.ChainIndex) as ChainId,
						msg.params.to,
						false,
					)),
					self.debug_mode,
					self.handle.clone(),
				);
			}
		}

		Ok(())
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}

#[async_trait::async_trait]
impl<F, P> SocketRelayBuilder<F, P> for OutboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	fn get_client(&self) -> Arc<EthClient<F, P>> {
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
				.to(*self.bfc_client.protocol_contracts.socket.address()),
			is_external,
		)));
	}
}

#[async_trait::async_trait]
impl<F, P> BootstrapHandler for OutboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
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
