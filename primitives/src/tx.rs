use std::{
	collections::BTreeMap,
	fmt::{Display, Formatter},
};

use alloy::{
	primitives::{Address, ChainId, B256, U256},
	rpc::types::TransactionRequest,
};
use bitcoincore_rpc::bitcoin::PublicKey;
use miniscript::bitcoin::{address::NetworkUnchecked, Address as BtcAddress, Txid};
use subxt::{
	ext::subxt_core::Error,
	tx::{DefaultPayload, Payload},
	Metadata,
};
use tokio::sync::mpsc::{error::SendError, UnboundedSender};

use crate::{
	constants::tx::{DEFAULT_TX_RETRIES, DEFAULT_TX_RETRY_INTERVAL_MS},
	eth::{GasCoefficient, SocketEventStatus},
	periodic::PriceResponse,
	substrate::{
		ApproveSetRefunds, SubmitExecutedRequest, SubmitRollbackPoll, SubmitSignedPsbt,
		SubmitSystemVaultKey, SubmitUnsignedPsbt, SubmitVaultKey, VaultKeyPresubmission,
	},
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
	pub fn new(
		btc_address: BtcAddress<NetworkUnchecked>,
		bfc_address: Address,
		txid: Txid,
		index: u32,
	) -> Self {
		Self { btc_address, bfc_address, txid, index }
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

#[derive(Clone, Debug)]
pub enum TxRequestMetadata {
	SocketRelay(SocketRelayMetadata),
	PriceFeed(PriceFeedMetadata),
	VSPPhase1(VSPPhase1Metadata),
	VSPPhase2(VSPPhase2Metadata),
	Heartbeat(HeartbeatMetadata),
	Flush(FlushMetadata),
	Rollback(RollbackMetadata),
	BitcoinSocketRelay(BitcoinRelayMetadata),
}

impl Display for TxRequestMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"{}",
			match self {
				TxRequestMetadata::SocketRelay(metadata) => metadata.to_string(),
				TxRequestMetadata::PriceFeed(metadata) => metadata.to_string(),
				TxRequestMetadata::VSPPhase1(metadata) => metadata.to_string(),
				TxRequestMetadata::VSPPhase2(metadata) => metadata.to_string(),
				TxRequestMetadata::Heartbeat(metadata) => metadata.to_string(),
				TxRequestMetadata::Flush(metadata) => metadata.to_string(),
				TxRequestMetadata::Rollback(metadata) => metadata.to_string(),
				TxRequestMetadata::BitcoinSocketRelay(metadata) => metadata.to_string(),
			}
		)
	}
}

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
pub struct ApproveSetRefundsMetadata {
	pub approved: usize,
	pub denied: usize,
}

impl ApproveSetRefundsMetadata {
	pub fn new(approved: usize, denied: usize) -> Self {
		Self { approved, denied }
	}
}

impl Display for ApproveSetRefundsMetadata {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "ApproveSetRefunds(approved: {}, denied: {})", self.approved, self.denied)
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
	ApproveSetRefunds(ApproveSetRefundsMetadata),
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
				XtRequestMetadata::ApproveSetRefunds(metadata) => metadata.to_string(),
			}
		)
	}
}

#[derive(Clone)]
pub enum XtRequest {
	SubmitSignedPsbt(DefaultPayload<SubmitSignedPsbt>),
	SubmitVaultKey(DefaultPayload<SubmitVaultKey>),
	SubmitUnsignedPsbt(DefaultPayload<SubmitUnsignedPsbt>),
	SubmitExecutedRequest(DefaultPayload<SubmitExecutedRequest>),
	SubmitSystemVaultKey(DefaultPayload<SubmitSystemVaultKey>),
	SubmitRollbackPoll(DefaultPayload<SubmitRollbackPoll>),
	VaultKeyPresubmission(DefaultPayload<VaultKeyPresubmission>),
	ApproveSetRefunds(DefaultPayload<ApproveSetRefunds>),
}

impl Payload for XtRequest {
	fn encode_call_data_to(&self, metadata: &Metadata, out: &mut Vec<u8>) -> Result<(), Error> {
		match self {
			XtRequest::SubmitSignedPsbt(call) => call.encode_call_data_to(metadata, out),
			XtRequest::SubmitVaultKey(call) => call.encode_call_data_to(metadata, out),
			XtRequest::SubmitUnsignedPsbt(call) => call.encode_call_data_to(metadata, out),
			XtRequest::SubmitExecutedRequest(call) => call.encode_call_data_to(metadata, out),
			XtRequest::SubmitSystemVaultKey(call) => call.encode_call_data_to(metadata, out),
			XtRequest::SubmitRollbackPoll(call) => call.encode_call_data_to(metadata, out),
			XtRequest::VaultKeyPresubmission(call) => call.encode_call_data_to(metadata, out),
			XtRequest::ApproveSetRefunds(call) => call.encode_call_data_to(metadata, out),
		}
	}
}

impl TryFrom<XtRequest> for DefaultPayload<SubmitSignedPsbt> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(call) => Ok(call),
			XtRequest::SubmitVaultKey(_) => Err(()),
			XtRequest::SubmitUnsignedPsbt(_) => Err(()),
			XtRequest::SubmitExecutedRequest(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(_) => Err(()),
			XtRequest::SubmitRollbackPoll(_) => Err(()),
			XtRequest::VaultKeyPresubmission(_) => Err(()),
			XtRequest::ApproveSetRefunds(_) => Err(()),
		}
	}
}

impl TryFrom<XtRequest> for DefaultPayload<SubmitVaultKey> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(_) => Err(()),
			XtRequest::SubmitVaultKey(call) => Ok(call),
			XtRequest::SubmitUnsignedPsbt(_) => Err(()),
			XtRequest::SubmitExecutedRequest(_) => Err(()),
			XtRequest::SubmitRollbackPoll(_) => Err(()),
			XtRequest::VaultKeyPresubmission(_) => Err(()),
			XtRequest::ApproveSetRefunds(_) => Err(()),
		}
	}
}

impl TryFrom<XtRequest> for DefaultPayload<SubmitUnsignedPsbt> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(_) => Err(()),
			XtRequest::SubmitVaultKey(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(_) => Err(()),
			XtRequest::SubmitUnsignedPsbt(call) => Ok(call),
			XtRequest::SubmitExecutedRequest(_) => Err(()),
			XtRequest::SubmitRollbackPoll(_) => Err(()),
			XtRequest::VaultKeyPresubmission(_) => Err(()),
			XtRequest::ApproveSetRefunds(_) => Err(()),
		}
	}
}

impl TryFrom<XtRequest> for DefaultPayload<SubmitExecutedRequest> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(_) => Err(()),
			XtRequest::SubmitVaultKey(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(_) => Err(()),
			XtRequest::SubmitUnsignedPsbt(_) => Err(()),
			XtRequest::SubmitExecutedRequest(call) => Ok(call),
			XtRequest::SubmitRollbackPoll(_) => Err(()),
			XtRequest::VaultKeyPresubmission(_) => Err(()),
			XtRequest::ApproveSetRefunds(_) => Err(()),
		}
	}
}
impl TryFrom<XtRequest> for DefaultPayload<SubmitSystemVaultKey> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(_) => Err(()),
			XtRequest::SubmitVaultKey(_) => Err(()),
			XtRequest::SubmitUnsignedPsbt(_) => Err(()),
			XtRequest::SubmitExecutedRequest(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(call) => Ok(call),
			XtRequest::SubmitRollbackPoll(_) => Err(()),
			XtRequest::VaultKeyPresubmission(_) => Err(()),
			XtRequest::ApproveSetRefunds(_) => Err(()),
		}
	}
}
impl TryFrom<XtRequest> for DefaultPayload<SubmitRollbackPoll> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(_) => Err(()),
			XtRequest::SubmitVaultKey(_) => Err(()),
			XtRequest::SubmitUnsignedPsbt(_) => Err(()),
			XtRequest::SubmitExecutedRequest(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(_) => Err(()),
			XtRequest::SubmitRollbackPoll(call) => Ok(call),
			XtRequest::VaultKeyPresubmission(_) => Err(()),
			XtRequest::ApproveSetRefunds(_) => Err(()),
		}
	}
}

impl TryFrom<XtRequest> for DefaultPayload<VaultKeyPresubmission> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(_) => Err(()),
			XtRequest::SubmitVaultKey(_) => Err(()),
			XtRequest::SubmitUnsignedPsbt(_) => Err(()),
			XtRequest::SubmitExecutedRequest(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(_) => Err(()),
			XtRequest::SubmitRollbackPoll(_) => Err(()),
			XtRequest::VaultKeyPresubmission(call) => Ok(call),
			XtRequest::ApproveSetRefunds(_) => Err(()),
		}
	}
}

impl TryFrom<XtRequest> for DefaultPayload<ApproveSetRefunds> {
	type Error = ();

	fn try_from(value: XtRequest) -> Result<Self, Self::Error> {
		match value {
			XtRequest::SubmitSignedPsbt(_) => Err(()),
			XtRequest::SubmitVaultKey(_) => Err(()),
			XtRequest::SubmitUnsignedPsbt(_) => Err(()),
			XtRequest::SubmitExecutedRequest(_) => Err(()),
			XtRequest::SubmitSystemVaultKey(_) => Err(()),
			XtRequest::SubmitRollbackPoll(_) => Err(()),
			XtRequest::VaultKeyPresubmission(_) => Err(()),
			XtRequest::ApproveSetRefunds(call) => Ok(call),
		}
	}
}

impl From<DefaultPayload<SubmitSignedPsbt>> for XtRequest {
	fn from(value: DefaultPayload<SubmitSignedPsbt>) -> Self {
		Self::SubmitSignedPsbt(value)
	}
}
impl From<DefaultPayload<SubmitVaultKey>> for XtRequest {
	fn from(value: DefaultPayload<SubmitVaultKey>) -> Self {
		Self::SubmitVaultKey(value)
	}
}
impl From<DefaultPayload<SubmitUnsignedPsbt>> for XtRequest {
	fn from(value: DefaultPayload<SubmitUnsignedPsbt>) -> Self {
		Self::SubmitUnsignedPsbt(value)
	}
}
impl From<DefaultPayload<SubmitExecutedRequest>> for XtRequest {
	fn from(value: DefaultPayload<SubmitExecutedRequest>) -> Self {
		Self::SubmitExecutedRequest(value)
	}
}
impl From<DefaultPayload<SubmitSystemVaultKey>> for XtRequest {
	fn from(value: DefaultPayload<SubmitSystemVaultKey>) -> Self {
		Self::SubmitSystemVaultKey(value)
	}
}
impl From<DefaultPayload<SubmitRollbackPoll>> for XtRequest {
	fn from(value: DefaultPayload<SubmitRollbackPoll>) -> Self {
		Self::SubmitRollbackPoll(value)
	}
}
impl From<DefaultPayload<VaultKeyPresubmission>> for XtRequest {
	fn from(value: DefaultPayload<VaultKeyPresubmission>) -> Self {
		Self::VaultKeyPresubmission(value)
	}
}
impl From<DefaultPayload<ApproveSetRefunds>> for XtRequest {
	fn from(value: DefaultPayload<ApproveSetRefunds>) -> Self {
		Self::ApproveSetRefunds(value)
	}
}

#[derive(Clone, Debug)]
/// The message format passed through the event channel.
pub struct TxRequestMessage {
	/// The remaining retries of the transaction request.
	pub retries_remaining: u8,
	/// The retry interval in milliseconds.
	pub retry_interval: u64,
	/// The raw transaction request.
	pub tx_request: TransactionRequest,
	/// Additional data of the transaction request.
	pub metadata: TxRequestMetadata,
	/// Check mempool to prevent duplicate relay.
	pub check_mempool: bool,
	/// The flag that represents whether the event is processed to an external chain.
	pub give_random_delay: bool,
	/// The gas coefficient that will be multiplied to the estimated gas amount.
	pub gas_coefficient: GasCoefficient,
	/// The flag that represents whether the event is requested by a bootstrap process.
	/// If true, the event will be processed by a asynchronous task.
	pub is_bootstrap: bool,
}

impl TxRequestMessage {
	/// Instantiates a new `TxRequestMessage` instance.
	pub fn new(
		tx_request: TransactionRequest,
		metadata: TxRequestMetadata,
		check_mempool: bool,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
		is_bootstrap: bool,
	) -> Self {
		Self {
			retries_remaining: DEFAULT_TX_RETRIES,
			retry_interval: DEFAULT_TX_RETRY_INTERVAL_MS,
			tx_request,
			metadata,
			check_mempool,
			give_random_delay,
			gas_coefficient,
			is_bootstrap,
		}
	}

	/// Builds a new `TxRequestMessage` to use on transaction retry. This will reduce the remaining
	/// retry counter and increase the retry interval.
	pub fn build_retry_event(&mut self) {
		self.tx_request.nonce = None;
		self.retries_remaining = self.retries_remaining.saturating_sub(1);
	}
}

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
