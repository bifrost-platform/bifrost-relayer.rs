use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::{EthClient, send_transaction},
};

use alloy::{
	network::AnyNetwork,
	primitives::{Address as EthAddress, B256, ChainId, U256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
	rpc::types::{TransactionInput, TransactionRequest},
};
use bitcoincore_rpc::bitcoin::Txid;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::bitcoin_socket::BitcoinSocketInstance,
	tx::{BitcoinRelayMetadata, TxRequestMetadata},
	utils::sub_display_format,
};
use eyre::Result;

use miniscript::bitcoin::{Address as BtcAddress, address::NetworkUnchecked, hashes::Hash};
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use super::{BootstrapHandler, EventMessage};

const SUB_LOG_TARGET: &str = "inbound-handler";

pub struct InboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// `EthClient` for interact with Bifrost network.
	pub bfc_client: Arc<EthClient<F, P>>,
	/// The receiver that consumes new events from the block channel.
	event_stream: BroadcastStream<BTCEventMessage>,
	/// Event type which this handler should handle.
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
	/// Whether to enable debug mode.
	debug_mode: bool,
}

impl<F, P> InboundHandler<F, P>
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
			target_event: EventType::Inbound,
			bootstrap_shared_data,
			handle,
			debug_mode,
		}
	}

	async fn get_user_bfc_address(
		&self,
		vault_address: &BtcAddress<NetworkUnchecked>,
	) -> Result<Option<EthAddress>> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

		let vault_address = vault_address.clone().assume_checked().to_string();
		let round = self.get_current_round().await?;
		let user_address = registration_pool.user_address(vault_address, round).call().await?._0;

		if user_address == EthAddress::ZERO { Ok(None) } else { Ok(Some(user_address)) }
	}

	async fn is_rollback_output(&self, txid: Txid, index: u32) -> Result<bool> {
		let socket_queue = self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap();

		let slice: [u8; 32] = txid.to_byte_array();
		let psbt_txid =
			socket_queue.rollback_output(slice.into(), U256::from(index)).call().await?._0;

		Ok(!B256::from(psbt_txid).is_zero())
	}

	fn build_transaction(&self, event: &Event, user_bfc_address: EthAddress) -> TransactionRequest {
		let calldata = self
			.bitcoin_socket()
			.poll(
				event.txid.to_byte_array().into(),
				U256::from(event.index),
				user_bfc_address,
				U256::from(event.amount.to_sat()),
			)
			.calldata()
			.clone();

		TransactionRequest::default()
			.input(TransactionInput::new(calldata))
			.to(*self.bitcoin_socket().address())
	}

	/// Checks if the vote for a request has already finished.
	async fn is_vote_finished(&self, event: &Event, user_bfc_address: EthAddress) -> Result<bool> {
		let hash_key = self
			.bitcoin_socket()
			.getHashKey(
				event.txid.to_byte_array().into(),
				U256::from(event.index),
				user_bfc_address,
				U256::from(event.amount.to_sat()),
			)
			.call()
			.await?
			._0;

		let tx_info = self.bitcoin_socket().txs(hash_key).call().await?;
		if tx_info.voteCount
			>= self.bfc_client.protocol_contracts.authority.majority_0().call().await?._0
		{
			// a vote for a request has already finished
			return Ok(true);
		}

		// check if the relayer has voted for this request
		Ok(self
			.bitcoin_socket()
			.isRelayerVoted(hash_key, self.bfc_client.address().await)
			.call()
			.await?
			._0)
	}

	async fn get_current_round(&self) -> Result<u32> {
		let registration_pool =
			self.bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();
		Ok(registration_pool.current_round().call().await?._0)
	}

	#[inline]
	fn bitcoin_socket(&self) -> &BitcoinSocketInstance<F, P> {
		self.bfc_client.protocol_contracts.bitcoin_socket.as_ref().unwrap()
	}

	async fn submit_utxo(&self) -> Result<()> {
		todo!("implement this")
	}
}

#[async_trait::async_trait]
impl<F, P> Handler for InboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<AnyNetwork> + 'static,
{
	async fn run(&mut self) -> Result<()> {
		self.wait_for_all_chains_bootstrapped().await?;
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

	async fn process_event(&self, mut event: Event) -> Result<()> {
		if let Some(user_bfc_address) = self.get_user_bfc_address(&event.address).await? {
			// txid from event is in little endian, convert it to big endian
			event.txid = {
				let mut slice: [u8; 32] = event.txid.to_byte_array();
				slice.reverse();
				Txid::from_slice(&slice).unwrap()
			};

			// check if transaction has been submitted to be rollbacked
			if self.is_rollback_output(event.txid, event.index).await? {
				return Ok(());
			}
			// check if vote for this request has already finished or if the relayer has voted for this request
			if self.is_vote_finished(&event, user_bfc_address).await? {
				return Ok(());
			}

			let tx_request = self.build_transaction(&event, user_bfc_address);
			let metadata =
				BitcoinRelayMetadata::new(event.address, user_bfc_address, event.txid, event.index);

			send_transaction(
				self.bfc_client.clone(),
				tx_request,
				format!("{} ({})", SUB_LOG_TARGET, self.bfc_client.get_chain_name()),
				TxRequestMetadata::BitcoinSocketRelay(metadata),
				self.debug_mode,
				self.handle.clone(),
			);

			if self.bfc_client.blaze_activation().await? {
				self.submit_utxo().await?;
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
impl<F, P> BootstrapHandler for InboundHandler<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	fn get_chain_id(&self) -> ChainId {
		self.bfc_client.get_bitcoin_chain_id().unwrap()
	}

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
