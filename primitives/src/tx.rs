use std::{
	collections::BTreeMap,
	fmt::{Display, Formatter},
	sync::Arc,
};

use alloy::primitives::{Address, B256, Bytes, ChainId, U256};
use bitcoincore_rpc::bitcoin::PublicKey;
use miniscript::bitcoin::{Address as BtcAddress, Amount, Txid, address::NetworkUnchecked};
use subxt::tx::Payload;
use tokio::sync::mpsc::{UnboundedSender, error::SendError};

use crate::{
	btc::Event as BtcEvent,
	constants::tx::{DEFAULT_TX_RETRIES, DEFAULT_TX_RETRY_INTERVAL_MS},
	eth::SocketEventStatus,
	periodic::PriceResponse,
};

#[derive(Clone, Debug)]
pub struct SocketRelayMetadata {
	/// The socket relay direction flag.
	pub is_inbound: bool,
	/// The socket event status.
	pub status: SocketEventStatus,
	/// The socket request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: ChainId,
	/// The destination chain ID.
	pub dst_chain_id: ChainId,
	/// The receiver address for this request.
	pub receiver: Address,
	/// The flag whether this relay is processed on bootstrap.
	pub is_bootstrap: bool,
}

impl SocketRelayMetadata {
	pub fn new(
		is_inbound: bool,
		status: SocketEventStatus,
		sequence: u128,
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
		receiver: Address,
		is_bootstrap: bool,
	) -> Self {
		Self { is_inbound, status, sequence, src_chain_id, dst_chain_id, receiver, is_bootstrap }
	}
}

impl Display for SocketRelayMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Relay({}-{:?}-{:?}, {:?} -> {:?})",
			if self.is_inbound { "Inbound".to_string() } else { "Outbound".to_string() },
			self.status,
			self.sequence,
			self.src_chain_id,
			self.dst_chain_id,
		)
	}
}

#[derive(Clone, Debug)]
pub struct PriceFeedMetadata {
	/// The fetched price responses mapped to token symbol.
	pub prices: BTreeMap<String, PriceResponse>,
}

impl PriceFeedMetadata {
	pub fn new(prices: BTreeMap<String, PriceResponse>) -> Self {
		Self { prices }
	}
}

impl Display for PriceFeedMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "PriceFeed({:?})", self.prices)
	}
}

#[derive(Clone, Debug)]
pub struct VSPPhase1Metadata {
	/// The round index to update.
	pub round: U256,
	/// The relayer addresses of the round.
	pub relayer_addresses: Vec<Address>,
}

impl VSPPhase1Metadata {
	pub fn new(round: U256, relayer_addresses: Vec<Address>) -> Self {
		Self { round, relayer_addresses }
	}
}

impl Display for VSPPhase1Metadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let relayers = self
			.relayer_addresses
			.iter()
			.map(|address| address.to_string())
			.collect::<Vec<String>>();
		write!(f, "VSPPhase1({:?}, {:?})", self.round, relayers)
	}
}

#[derive(Clone, Debug)]
pub struct VSPPhase2Metadata {
	/// The round index to update.
	pub round: U256,
	/// The destination chain ID to update.
	pub dst_chain_id: ChainId,
}

impl VSPPhase2Metadata {
	pub fn new(round: U256, dst_chain_id: ChainId) -> Self {
		Self { round, dst_chain_id }
	}
}

impl Display for VSPPhase2Metadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "VSPPhase2({:?}, On chain:{:?})", self.round, self.dst_chain_id)
	}
}

#[derive(Clone, Debug)]
pub struct HeartbeatMetadata {
	/// The current round index.
	pub current_round_index: U256,
	/// The current session index.
	pub current_session_index: U256,
}

impl HeartbeatMetadata {
	pub fn new(current_round_index: U256, current_session_index: U256) -> Self {
		Self { current_round_index, current_session_index }
	}
}

impl Display for HeartbeatMetadata {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Heartbeat(Round: {:?}, Session: {:?})",
			self.current_round_index, self.current_session_index
		)
	}
}

#[derive(Clone, Debug)]
pub struct FlushMetadata {}

impl FlushMetadata {
	pub fn new() -> Self {
		Self {}
	}
}

impl Default for FlushMetadata {
	fn default() -> Self {
		Self::new()
	}
}

impl Display for FlushMetadata {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "Flush mempool")
	}
}

#[derive(Clone, Debug)]
pub struct RollbackMetadata {
	/// The socket relay direction flag.
	pub is_inbound: bool,
	/// The socket event status.
	pub status: SocketEventStatus,
	/// The socket request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: ChainId,
	/// The destination chain ID.
	pub dst_chain_id: ChainId,
}

impl RollbackMetadata {
	pub fn new(
		is_inbound: bool,
		status: SocketEventStatus,
		sequence: u128,
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
	) -> Self {
		Self { is_inbound, status, sequence, src_chain_id, dst_chain_id }
	}
}

impl Display for RollbackMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Rollback({}-{:?}-{:?}, {:?} -> {:?})",
			if self.is_inbound { "Inbound".to_string() } else { "Outbound".to_string() },
			self.status,
			self.sequence,
			self.src_chain_id,
			self.dst_chain_id,
		)
	}
}

#[derive(Clone, Debug)]
pub struct BitcoinRelayMetadata {
	pub btc_address: BtcAddress<NetworkUnchecked>,
	pub bfc_address: Address,
	pub txid: Txid,
	pub index: u32,
}

impl BitcoinRelayMetadata {
	pub fn new(event: &BtcEvent, bfc_address: Address) -> Self {
		Self {
			btc_address: event.address.clone(),
			bfc_address,
			txid: event.txid,
			index: event.index,
		}
	}
}

impl Display for BitcoinRelayMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"BitcoinSocketRelay({}:{} {:?}:{})",
			self.txid, self.index, self.btc_address, self.bfc_address,
		)
	}
}

pub type TxRequestMetadata = Arc<dyn Display + Send + Sync>;

#[derive(Clone, Debug)]
/// The metadata used for vault key submission.
pub struct SubmitVaultKeyMetadata {
	/// The user's Bifrost address.
	pub who: Address,
	/// The generated public key.
	pub key: PublicKey,
}

impl SubmitVaultKeyMetadata {
	pub fn new(who: Address, key: PublicKey) -> Self {
		Self { who, key }
	}
}

impl Display for SubmitVaultKeyMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "SubmitVaultKey({:?}:{})", self.who, self.key)
	}
}

#[derive(Clone, Debug)]
/// The metadata used for signed psbt submission.
pub struct SubmitSignedPsbtMetadata {
	pub unsigned_psbt: B256,
}

impl SubmitSignedPsbtMetadata {
	pub fn new(unsigned_psbt: B256) -> Self {
		Self { unsigned_psbt }
	}
}

impl Display for SubmitSignedPsbtMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "SubmitSignedPsbt({})", self.unsigned_psbt)
	}
}

#[derive(Clone, Debug)]
/// The metadata used for unsigned psbt submission.
pub struct SubmitUnsignedPsbtMetadata {
	pub unsigned_psbt: B256,
}

impl SubmitUnsignedPsbtMetadata {
	pub fn new(unsigned_psbt: B256) -> Self {
		Self { unsigned_psbt }
	}
}

impl Display for SubmitUnsignedPsbtMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "SubmitUnsignedPsbt({})", self.unsigned_psbt)
	}
}

#[derive(Clone, Debug)]
pub struct SubmitExecutedRequestMetadata {
	pub txid: B256,
}

impl SubmitExecutedRequestMetadata {
	pub fn new(txid: B256) -> Self {
		Self { txid }
	}
}

impl Display for SubmitExecutedRequestMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "SubmitExecutedRequest({})", self.txid)
	}
}

#[derive(Clone, Debug)]
/// The metadata used for rollback poll submission.
pub struct SubmitRollbackPollMetadata {
	pub txid: B256,
	pub is_approved: bool,
}

impl SubmitRollbackPollMetadata {
	pub fn new(txid: B256, is_approved: bool) -> Self {
		Self { txid, is_approved }
	}
}

impl Display for SubmitRollbackPollMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "SubmitRollbackPoll({}:{})", self.txid, self.is_approved)
	}
}

#[derive(Clone, Debug)]
pub struct VaultKeyPresubmissionMetadata {
	pub keys: usize,
}

impl VaultKeyPresubmissionMetadata {
	pub fn new(keys: usize) -> Self {
		Self { keys }
	}
}

impl Display for VaultKeyPresubmissionMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "VaultKeyPresubmission({})", self.keys)
	}
}

#[derive(Clone, Debug)]
pub struct SubmitUtxoMetadata {
	pub txid: Txid,
	pub vout: u32,
	pub amount: Amount,
}

impl SubmitUtxoMetadata {
	pub fn new(event: &BtcEvent) -> Self {
		Self { txid: event.txid, vout: event.index, amount: event.amount }
	}
}

impl Display for SubmitUtxoMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "SubmitUtxo({}:{}:{})", self.txid, self.vout, self.amount)
	}
}

#[derive(Clone, Debug)]
pub struct BroadcastPollMetadata {
	pub txid: Txid,
}

impl BroadcastPollMetadata {
	pub fn new(txid: Txid) -> Self {
		Self { txid }
	}
}

impl Display for BroadcastPollMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "BroadcastPoll({})", self.txid)
	}
}

#[derive(Clone, Debug)]
pub struct SubmitFeeRateMetadata {
	pub lt_fee_rate: u64,
	pub fee_rate: u64,
}

impl SubmitFeeRateMetadata {
	pub fn new(lt_fee_rate: u64, fee_rate: u64) -> Self {
		Self { lt_fee_rate, fee_rate }
	}
}

impl Display for SubmitFeeRateMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "SubmitFeeRate({},{})", self.lt_fee_rate, self.fee_rate)
	}
}

#[derive(Clone, Debug)]
pub struct RemoveOutboundMessagesMetadata {
	pub len: usize,
}

impl RemoveOutboundMessagesMetadata {
	pub fn new(len: usize) -> Self {
		Self { len }
	}
}

impl Display for RemoveOutboundMessagesMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "RemoveOutboundMessages({})", self.len)
	}
}

#[derive(Clone, Debug)]
pub struct HookMetadata {
	pub sender: Address,
	pub receiver: Address,
	pub max_tx_fee: U256,
	pub fee_in_bridged_asset: Option<U256>,
	pub message: Bytes,
}

impl HookMetadata {
	pub fn new(
		sender: Address,
		receiver: Address,
		max_tx_fee: U256,
		fee_in_bridged_asset: Option<U256>,
		message: Bytes,
	) -> Self {
		Self { sender, receiver, max_tx_fee, fee_in_bridged_asset, message }
	}
}

impl Display for HookMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Hook({:?}, {:?}, {:?}, {:?}, {:?})",
			self.sender,
			self.receiver,
			self.max_tx_fee,
			self.fee_in_bridged_asset.unwrap_or(U256::ZERO),
			self.message
		)
	}
}

#[derive(Clone, Debug)]
pub struct OnFlightPollMetadata {
	/// The socket relay direction flag.
	pub is_inbound: bool,
	/// The socket request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: ChainId,
	/// The destination chain ID.
	pub dst_chain_id: ChainId,
}

impl OnFlightPollMetadata {
	pub fn new(
		is_inbound: bool,
		sequence: u128,
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
	) -> Self {
		Self { is_inbound, sequence, src_chain_id, dst_chain_id }
	}
}

impl Display for OnFlightPollMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"OnFlightPoll({}-{}, {:?} -> {:?})",
			if self.is_inbound { "Inbound" } else { "Outbound" },
			self.sequence,
			self.src_chain_id,
			self.dst_chain_id,
		)
	}
}

#[derive(Clone, Debug)]
pub struct FinalizePollMetadata {
	/// The socket relay direction flag.
	pub is_inbound: bool,
	/// The socket request sequence ID.
	pub sequence: u128,
	/// The source chain ID.
	pub src_chain_id: ChainId,
	/// The destination chain ID.
	pub dst_chain_id: ChainId,
	/// Whether the transfer was committed (true) or rollbacked (false).
	pub is_committed: bool,
}

impl FinalizePollMetadata {
	pub fn new(
		is_inbound: bool,
		sequence: u128,
		src_chain_id: ChainId,
		dst_chain_id: ChainId,
		is_committed: bool,
	) -> Self {
		Self { is_inbound, sequence, src_chain_id, dst_chain_id, is_committed }
	}
}

impl Display for FinalizePollMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"FinalizePoll({}-{}, {:?} -> {:?}, {})",
			if self.is_inbound { "Inbound" } else { "Outbound" },
			self.sequence,
			self.src_chain_id,
			self.dst_chain_id,
			if self.is_committed { "Committed" } else { "Rollbacked" },
		)
	}
}

#[derive(Clone)]
pub enum XtRequestMetadata {
	SubmitVaultKey(SubmitVaultKeyMetadata),
	SubmitSignedPsbt(SubmitSignedPsbtMetadata),
	SubmitUnsignedPsbt(SubmitUnsignedPsbtMetadata),
	SubmitExecutedRequest(SubmitExecutedRequestMetadata),
	SubmitRollbackPoll(SubmitRollbackPollMetadata),
	VaultKeyPresubmission(VaultKeyPresubmissionMetadata),
	SubmitUtxos(SubmitUtxoMetadata),
	BroadcastPoll(BroadcastPollMetadata),
	SubmitOutboundRequests(SocketRelayMetadata),
	SubmitFeeRate(SubmitFeeRateMetadata),
	RemoveOutboundMessages(RemoveOutboundMessagesMetadata),
	OnFlightPoll(OnFlightPollMetadata),
	FinalizePoll(FinalizePollMetadata),
}

impl Display for XtRequestMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"{}",
			match self {
				XtRequestMetadata::SubmitVaultKey(metadata) => metadata.to_string(),
				XtRequestMetadata::SubmitSignedPsbt(metadata) => metadata.to_string(),
				XtRequestMetadata::SubmitUnsignedPsbt(metadata) => metadata.to_string(),
				XtRequestMetadata::SubmitExecutedRequest(metadata) => metadata.to_string(),
				XtRequestMetadata::SubmitRollbackPoll(metadata) => metadata.to_string(),
				XtRequestMetadata::VaultKeyPresubmission(metadata) => metadata.to_string(),
				XtRequestMetadata::SubmitUtxos(metadata) => metadata.to_string(),
				XtRequestMetadata::BroadcastPoll(metadata) => metadata.to_string(),
				XtRequestMetadata::SubmitOutboundRequests(metadata) => metadata.to_string(),
				XtRequestMetadata::SubmitFeeRate(metadata) => metadata.to_string(),
				XtRequestMetadata::RemoveOutboundMessages(metadata) => metadata.to_string(),
				XtRequestMetadata::OnFlightPoll(metadata) => metadata.to_string(),
				XtRequestMetadata::FinalizePoll(metadata) => metadata.to_string(),
			}
		)
	}
}

pub type XtRequest = Arc<dyn Payload + Send + Sync>;

pub struct XtRequestMessage {
	/// The remaining retries of the transaction request.
	pub retries_remaining: u8,
	/// The retry interval in milliseconds.
	pub retry_interval: u64,
	/// The call data of the transaction.
	pub call: XtRequest,
	/// The metadata of the transaction.
	pub metadata: XtRequestMetadata,
}

impl XtRequestMessage {
	/// Instantiates a new `XtRequestMessage` instance.
	pub fn new(call: XtRequest, metadata: XtRequestMetadata) -> Self {
		Self {
			retries_remaining: DEFAULT_TX_RETRIES,
			retry_interval: DEFAULT_TX_RETRY_INTERVAL_MS,
			call,
			metadata,
		}
	}

	/// Builds a new `XtRequestMessage` to use on transaction retry. This will reduce the remaining
	/// retry counter and increase the retry interval.
	pub fn build_retry_event(&mut self) {
		self.retries_remaining = self.retries_remaining.saturating_sub(1);
	}
}

/// The message sender connected to the tx request channel.
pub struct XtRequestSender {
	/// The inner sender.
	pub sender: UnboundedSender<XtRequestMessage>,
}

impl XtRequestSender {
	/// Instantiates a new `XtRequestSender` instance.
	pub fn new(sender: UnboundedSender<XtRequestMessage>) -> Self {
		Self { sender }
	}

	/// Sends a new tx request message.
	pub fn send(&self, message: XtRequestMessage) -> Result<(), Box<SendError<XtRequestMessage>>> {
		Ok(self.sender.send(message)?)
	}
}
