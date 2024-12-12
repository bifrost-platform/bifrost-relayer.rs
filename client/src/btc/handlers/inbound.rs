use crate::{
	btc::{
		block::{Event, EventMessage as BTCEventMessage, EventType},
		handlers::{Handler, LOG_TARGET},
	},
	eth::{send_transaction, EthClient},
};

use alloy::{
	network::AnyNetwork,
	primitives::{Address as EthAddress, B256, U256},
	providers::{
		fillers::{FillProvider, TxFiller},
		Provider, WalletProvider,
	},
	rpc::types::{TransactionInput, TransactionRequest},
	transports::Transport,
};
use bitcoincore_rpc::bitcoin::Txid;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::bitcoin_socket::BitcoinSocketContract::BitcoinSocketContractInstance,
	eth::BootstrapState,
	tx::{BitcoinRelayMetadata, TxRequestMetadata},
	utils::sub_display_format,
};
use eyre::Result;

use miniscript::bitcoin::{address::NetworkUnchecked, hashes::Hash, Address as BtcAddress};
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

use super::{BootstrapHandler, EventMessage};

const SUB_LOG_TARGET: &str = "inbound-handler";

pub struct InboundHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// `EthClient` for interact with Bifrost network.
	pub bfc_client: Arc<EthClient<F, P, T>>,
	/// The receiver that consumes new events from the block channel.
	event_receiver: Receiver<BTCEventMessage>,
	/// Event type which this handler should handle.
	target_event: EventType,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
}

impl<F, P, T> InboundHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	pub fn new(
		bfc_client: Arc<EthClient<F, P, T>>,
		event_receiver: Receiver<BTCEventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		handle: SpawnTaskHandle,
	) -> Self {
		Self {
			bfc_client,
			event_receiver,
			target_event: EventType::Inbound,
			bootstrap_shared_data,
			handle,
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

		if user_address == EthAddress::ZERO {
			Ok(None)
		} else {
			Ok(Some(user_address))
		}
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
			.to(self.bitcoin_socket().address().clone())
	}

	/// Checks if the relayer has already voted on the event.
	async fn is_relayer_voted(&self, event: &Event, user_bfc_address: EthAddress) -> Result<bool> {
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

		Ok(self
			.bitcoin_socket()
			.isRelayerVoted(hash_key, self.bfc_client.address())
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
	fn bitcoin_socket(
		&self,
	) -> &BitcoinSocketContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork> {
		self.bfc_client.protocol_contracts.bitcoin_socket.as_ref().unwrap()
	}
}

#[async_trait::async_trait]
impl<F, P, T> Handler for InboundHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
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

				let mut stream = tokio_stream::iter(msg.events);
				while let Some(event) = stream.next().await {
					self.process_event(event).await?;
				}
			}
		}
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
			if self.is_relayer_voted(&event, user_bfc_address).await? {
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
				self.handle.clone(),
			);
		}

		Ok(())
	}

	#[inline]
	fn is_target_event(&self, event_type: EventType) -> bool {
		event_type == self.target_event
	}
}

#[async_trait::async_trait]
impl<F, P, T> BootstrapHandler for InboundHandler<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
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
