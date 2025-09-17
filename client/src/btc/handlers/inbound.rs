use crate::{
	btc::handlers::{Handler, LOG_TARGET},
	eth::{EthClient, send_transaction},
};

use super::{BootstrapHandler, XtRequester};
use alloy::{
	network::Network,
	primitives::{Address as EthAddress, B256, ChainId, U256, keccak256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use bitcoincore_rpc::bitcoin::Txid;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	btc::{Event, EventMessage as BTCEventMessage, EventType},
	contracts::{bitcoin_socket::BitcoinSocketInstance, blaze::BlazeInstance},
	substrate::{BoundedVec, EthereumSignature, UtxoInfo, UtxoSubmission, bifrost_runtime},
	tx::{
		BitcoinRelayMetadata, SubmitUtxoMetadata, TxRequestMetadata, XtRequest, XtRequestMessage,
		XtRequestMetadata, XtRequestSender,
	},
	utils::sub_display_format,
};
use eyre::Result;
use miniscript::bitcoin::{Address as BtcAddress, address::NetworkUnchecked, hashes::Hash};
use parity_scale_codec::Encode;
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use subxt::ext::subxt_core::utils::AccountId20;
use tokio::sync::broadcast::Receiver;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

const SUB_LOG_TARGET: &str = "inbound-handler";

pub struct InboundHandler<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// `EthClient` to interact with Bifrost network.
	pub bfc_client: Arc<EthClient<F, P, N>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
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

impl<F, P, N: Network> InboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub fn new(
		bfc_client: Arc<EthClient<F, P, N>>,
		xt_request_sender: Arc<XtRequestSender>,
		event_receiver: Receiver<BTCEventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
	) -> Self {
		Self {
			bfc_client,
			xt_request_sender,
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
		let user_address = registration_pool.user_address(vault_address, 0).call().await?;

		// if system vault address, return None.
		// otherwise, return Some(user address).
		if user_address == *registration_pool.address() { Ok(None) } else { Ok(Some(user_address)) }
	}

	async fn is_rollback_output(&self, txid: Txid, index: u32) -> Result<bool> {
		let socket_queue = self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap();

		let slice: [u8; 32] = txid.to_byte_array();
		let psbt_txid =
			socket_queue.rollback_output(slice.into(), U256::from(index)).call().await?;

		Ok(!B256::from(psbt_txid).is_zero())
	}

	fn build_transaction(
		&self,
		event: &Event,
		user_bfc_address: EthAddress,
	) -> N::TransactionRequest {
		self.bitcoin_socket()
			.poll(
				event.txid.to_byte_array().into(),
				U256::from(event.index),
				user_bfc_address,
				U256::from(event.amount.to_sat()),
			)
			.into_transaction_request()
	}

	/// Build the payload for the unsigned transaction. (`submit_utxos()`)
	async fn build_payload(
		&self,
		event: &Event,
	) -> Result<(UtxoSubmission<AccountId20>, EthereumSignature)> {
		let txid: subxt::utils::H256 = event.txid.to_byte_array().into();
		let msg = UtxoSubmission {
			authority_id: AccountId20(self.bfc_client.address().await.0.0),
			utxos: vec![UtxoInfo {
				txid,
				vout: event.index,
				amount: event.amount.to_sat(),
				address: BoundedVec(event.address.assume_checked_ref().to_string().into_bytes()),
			}],
		};
		let utxo_hash = keccak256(Encode::encode(&(txid, event.index, event.amount.to_sat())));

		let signature = self
			.bfc_client
			.sign_message(
				&[
					keccak256("UtxosSubmission").as_slice(),
					array_bytes::bytes2hex("", utxo_hash).as_bytes(),
				]
				.concat(),
			)
			.await?
			.into();

		Ok((msg, signature))
	}

	/// Build the calldata for the unsigned transaction. (`submit_utxos()`)
	async fn build_unsigned_tx(&self, event: &Event) -> Result<(XtRequest, SubmitUtxoMetadata)> {
		let (msg, signature) = self.build_payload(event).await?;
		let metadata = SubmitUtxoMetadata::new(event);
		Ok((XtRequest::from(bifrost_runtime::tx().blaze().submit_utxos(msg, signature)), metadata))
	}

	/// Send the transaction request message to the channel.
	async fn request_send_transaction(&self, call: XtRequest, metadata: SubmitUtxoMetadata) {
		match self
			.xt_request_sender
			.send(XtRequestMessage::new(call, XtRequestMetadata::SubmitUtxos(metadata.clone())))
		{
			Ok(_) => log::info!(
				target: &self.bfc_client.get_chain_name(),
				"-[{}] 🔖 Request unsigned transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => {
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to send unsigned transaction: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.bfc_client.address().await,
					metadata,
					error
				);
				log::error!(target: &self.bfc_client.get_chain_name(), "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", &self.bfc_client.get_chain_name()),
					sentry::Level::Error,
				);
			},
		}
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
			.await?;

		let tx_info = self.bitcoin_socket().txs(hash_key).call().await?;
		if tx_info.voteCount
			>= self.bfc_client.protocol_contracts.authority.majority_0().call().await?
		{
			// a vote for a request has already finished
			return Ok(true);
		}

		// check if the relayer has voted for this request
		Ok(self
			.bitcoin_socket()
			.isRelayerVoted(hash_key, self.bfc_client.address().await)
			.call()
			.await?)
	}

	#[inline]
	fn bitcoin_socket(&self) -> &BitcoinSocketInstance<F, P, N> {
		self.bfc_client.protocol_contracts.bitcoin_socket.as_ref().unwrap()
	}

	#[inline]
	fn blaze(&self) -> &BlazeInstance<F, P, N> {
		self.bfc_client.protocol_contracts.blaze.as_ref().unwrap()
	}

	async fn submit_utxo(&self, event: &Event) -> Result<()> {
		if self
			.blaze()
			.is_submittable_utxo(
				event.txid.to_byte_array().into(),
				U256::from(event.index),
				U256::from(event.amount.to_sat()),
				self.bfc_client().address().await,
			)
			.call()
			.await?
		{
			let (call, metadata) = self.build_unsigned_tx(event).await?;
			self.request_send_transaction(call, metadata).await;
		}
		Ok(())
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> Handler for InboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
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

			for mut event in msg.events {
				event.txid = {
					let mut slice: [u8; 32] = event.txid.to_byte_array();
					slice.reverse();
					Txid::from_slice(&slice)?
				};
				self.process_event(&event).await?;
			}
		}

		Ok(())
	}

	async fn process_event(&self, event: &Event) -> Result<()> {
		if self.bfc_client.blaze_activation().await? {
			self.submit_utxo(&event).await?;
		}

		if let Some(user_bfc_address) = self.get_user_bfc_address(&event.address).await? {
			// check if transaction has been submitted to be rollbacked
			if self.is_rollback_output(event.txid, event.index).await? {
				return Ok(());
			}
			// check if vote for this request has already finished or if the relayer has voted for this request
			if self.is_vote_finished(&event, user_bfc_address).await? {
				return Ok(());
			}

			let tx_request = self.build_transaction(&event, user_bfc_address);
			let metadata = BitcoinRelayMetadata::new(&event, user_bfc_address);

			send_transaction(
				self.bfc_client.clone(),
				tx_request,
				format!("{} ({})", SUB_LOG_TARGET, self.bfc_client.get_chain_name()),
				TxRequestMetadata::BitcoinSocketRelay(metadata),
				self.debug_mode,
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
impl<F, P, N: Network> BootstrapHandler for InboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
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

	async fn get_bootstrap_events(&self) -> Result<(BTCEventMessage, BTCEventMessage)> {
		unreachable!("unimplemented")
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> XtRequester<F, P, N> for InboundHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<F, P, N>> {
		self.bfc_client.clone()
	}
}
