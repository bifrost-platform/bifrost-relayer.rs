use crate::{
	btc::{
		failover::FailoverClient as BtcClient,
		handlers::{Handler, LOG_TARGET},
	},
	eth::{EthClient, send_transaction},
};

use super::{BootstrapHandler, XtRequester};
use alloy::{
	network::Network,
	primitives::{Address as EthAddress, B256, ChainId, U256, keccak256},
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use array_bytes::Hexify;
use bitcoincore_rpc::{RpcApi, bitcoin::Txid};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	btc::{Event, EventMessage as BTCEventMessage, EventType},
	contracts::{bitcoin_socket::BitcoinSocketInstance, blaze::BlazeInstance},
	substrate::{BoundedVec, EthereumSignature, UtxoInfo, UtxoSubmission, bifrost_runtime},
	tx::{
		BitcoinRelayMetadata, SubmitUtxoMetadata, XtRequest, XtRequestMessage, XtRequestMetadata,
		XtRequestSender,
	},
	utils::sub_display_format,
};
use eyre::Result;
use miniscript::bitcoin::{Address as BtcAddress, address::NetworkUnchecked, hashes::Hash};
use parity_scale_codec::Encode;
use sc_service::SpawnTaskHandle;
use std::sync::Arc;
use subxt::utils::eth::AccountId20;
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
	/// The Bitcoin client, to look up the inputs of deposits to whitelisted vaults.
	btc_client: BtcClient,
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
		btc_client: BtcClient,
		xt_request_sender: Arc<XtRequestSender>,
		event_receiver: Receiver<BTCEventMessage>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
	) -> Self {
		Self {
			bfc_client,
			btc_client,
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

	/// Whether the deposit may be polled. A deposit to a vault the Koyomi `BtcDepositRegistry`
	/// whitelists counts only when every input spends from the depositor registered for it;
	/// deposits to any other vault always count.
	async fn is_accepted_deposit(&self, event: &Event) -> Result<bool> {
		let Some(registry) = self.bfc_client.protocol_contracts.btc_deposit_registry.as_ref()
		else {
			return Ok(true);
		};
		let vault = event.address.assume_checked_ref().to_string();
		let answer = registry.vaultDepositor(vault.clone()).call().await?;
		if !answer.whitelisted {
			return Ok(true);
		}

		// unparsable when empty: none registered, or its KYC expired / is suspended / revoked
		let accepted = match answer.depositor.parse::<BtcAddress<NetworkUnchecked>>() {
			Ok(depositor) => {
				let tx = self.btc_client.get_raw_transaction_info(&rpc_txid(event), None).await?;
				let inputs = tx.vin.into_iter().map(|input| input.txid.zip(input.vout));
				spends_only_from(inputs, &depositor, |txid, vout| self.output_owner(txid, vout))
					.await?
			},
			Err(_) => false,
		};
		if !accepted {
			br_primitives::log_and_capture!(
				warn,
				&self.bfc_client.get_chain_name(),
				SUB_LOG_TARGET,
				self.bfc_client.address().await,
				"🚫 Refused deposit {}:{} to whitelisted vault {}: not sent from its depositor ({:?}). Not polled, roll it back",
				rpc_txid(event),
				event.index,
				vault,
				answer.depositor
			);
		}
		Ok(accepted)
	}

	/// The address the output `txid:vout` pays to (`None` if it has none).
	async fn output_owner(
		&self,
		txid: Txid,
		vout: u32,
	) -> Result<Option<BtcAddress<NetworkUnchecked>>> {
		Ok(self
			.btc_client
			.get_raw_transaction_info(&txid, None)
			.await?
			.vout
			.into_iter()
			.nth(vout as usize)
			.and_then(|output| output.script_pub_key.address))
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
		let address = BoundedVec(event.address.assume_checked_ref().to_string().into_bytes());
		let utxo_hash =
			keccak256(Encode::encode(&(txid, event.index, event.amount.to_sat(), address)));

		let signature = self
			.bfc_client
			.sign_message(
				&[keccak256("UtxosSubmission").as_slice(), utxo_hash.hexify().as_bytes()].concat(),
			)
			.await?
			.into();

		Ok((msg, signature))
	}

	/// Build the calldata for the unsigned transaction. (`submit_utxos()`)
	async fn build_unsigned_tx(&self, event: &Event) -> Result<(XtRequest, SubmitUtxoMetadata)> {
		let (msg, signature) = self.build_payload(event).await?;
		let metadata = SubmitUtxoMetadata::new(event);
		Ok((
			XtRequest::SubmitUtxos(bifrost_runtime::tx().blaze().submit_utxos(msg, signature)),
			metadata,
		))
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
				br_primitives::log_and_capture!(
					error,
					&self.bfc_client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bfc_client.address().await,
					"❗️ Failed to send unsigned transaction: {}, Error: {}",
					metadata,
					error
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
				event.address.assume_checked_ref().to_string().into_bytes().into(),
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
			// a refused deposit is still submitted to blaze above, so it can be rolled back
			match self.is_accepted_deposit(event).await {
				Ok(true) => {},
				Ok(false) => return Ok(()),
				// skip only this deposit: an error would end `run` and drop the block's later ones
				Err(error) => {
					br_primitives::log_and_capture!(
						error,
						&self.bfc_client.get_chain_name(),
						SUB_LOG_TARGET,
						self.bfc_client.address().await,
						"❗️ Failed to check deposit {}:{} against its depositor, not polled: {}",
						rpc_txid(event),
						event.index,
						error
					);
					return Ok(());
				},
			}

			let tx_request = self.build_transaction(&event, user_bfc_address);
			let metadata = BitcoinRelayMetadata::new(&event, user_bfc_address);

			send_transaction(
				self.bfc_client.clone(),
				tx_request,
				format!("{} ({})", SUB_LOG_TARGET, self.bfc_client.get_chain_name()),
				Arc::new(metadata),
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

/// The event's txid as Bitcoin RPCs know it: `run` stores it byte-reversed for the contracts.
fn rpc_txid(event: &Event) -> Txid {
	let mut bytes = event.txid.to_byte_array();
	bytes.reverse();
	Txid::from_byte_array(bytes)
}

/// Whether every input (`txid:vout` it spends, `None` for a coinbase) spends from `depositor`,
/// looking each owner up with `owner_of`. Stops at the first that doesn't, so a deposit with
/// foreign inputs costs one lookup however many it has.
async fn spends_only_from<Fut>(
	inputs: impl IntoIterator<Item = Option<(Txid, u32)>>,
	depositor: &BtcAddress<NetworkUnchecked>,
	mut owner_of: impl FnMut(Txid, u32) -> Fut,
) -> Result<bool>
where
	Fut: Future<Output = Result<Option<BtcAddress<NetworkUnchecked>>>>,
{
	for input in inputs {
		let Some((txid, vout)) = input else {
			return Ok(false);
		};
		if owner_of(txid, vout).await?.as_ref() != Some(depositor) {
			return Ok(false);
		}
	}
	Ok(true)
}

#[cfg(test)]
mod tests {
	use super::*;

	const DEPOSITOR: &str = "bcrt1q7wtdpm9whw4qcqqkamxjky8jqp7e402nu0gd44";
	const OTHER: &str = "bcrt1q22vcxsa7mt6frxps53vyfjfea8pmm0yxlnl30djzksg8f42a0rrqf3zg7s";

	/// Checks inputs spending prev txs `[n; 32]` (`None`: a coinbase), whose output pays to
	/// DEPOSITOR for n = 1, OTHER for n = 2 and to no address otherwise. Returns the verdict
	/// and how many owners it looked up.
	async fn check(inputs: &[Option<u8>]) -> (bool, usize) {
		let depositor = DEPOSITOR.parse().unwrap();
		let inputs = inputs.iter().map(|n| n.map(|n| (Txid::from_byte_array([n; 32]), 0)));
		let mut lookups = 0;
		let accepted = spends_only_from(inputs, &depositor, |txid, _| {
			lookups += 1;
			let owner = match txid.to_byte_array()[0] {
				1 => Some(DEPOSITOR),
				2 => Some(OTHER),
				_ => None,
			};
			std::future::ready(Ok(owner.map(|address| address.parse().unwrap())))
		})
		.await
		.unwrap();
		(accepted, lookups)
	}

	#[tokio::test]
	async fn accepts_only_deposits_spent_entirely_from_the_depositor() {
		assert_eq!(check(&[Some(1), Some(1)]).await, (true, 2));
		assert_eq!(check(&[Some(1), Some(3)]).await, (false, 2));
		assert_eq!(check(&[None]).await, (false, 0));
	}

	#[tokio::test]
	async fn stops_at_the_first_foreign_input() {
		assert_eq!(check(&[Some(2), Some(1), Some(1), Some(1)]).await, (false, 1));
	}

	#[test]
	fn compares_bech32_depositors_regardless_of_case() {
		let depositor: BtcAddress<NetworkUnchecked> = DEPOSITOR.parse().unwrap();
		assert_eq!(
			DEPOSITOR.to_uppercase().parse::<BtcAddress<NetworkUnchecked>>().unwrap(),
			depositor
		);
		// mixed case is invalid bech32 (BIP173), so such a depositor refuses every deposit
		assert!(
			DEPOSITOR
				.replacen("bcrt1q", "bcrt1Q", 1)
				.parse::<BtcAddress<NetworkUnchecked>>()
				.is_err()
		);
	}
}
