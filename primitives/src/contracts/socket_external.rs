use ethers::{
	abi::RawLog,
	prelude::abigen,
	types::{Bytes, Signature, TransactionRequest, U256},
};

abigen!(
	SocketExternal,
	"../abi/abi.socket.external.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

#[derive(
	Clone,
	ethers::contract::EthCall,
	ethers::contract::EthDisplay,
	Default,
	Debug,
	PartialEq,
	Eq,
	Hash,
)]
#[ethcall(
	name = "poll",
	abi = "poll(((bytes4,uint64,uint128),uint8,(bytes4,bytes16),(bytes32,bytes32,address,address,uint256,bytes)),(bytes32[],bytes32[],bytes),uint256)"
)]
pub struct SerializedPoll {
	pub msg: SocketMessage,
	pub sigs: Signatures,
	pub option: U256,
}

#[derive(
	Clone,
	ethers::contract::EthEvent,
	ethers::contract::EthDisplay,
	Default,
	Debug,
	PartialEq,
	Eq,
	Hash,
)]
#[ethevent(
	name = "Socket",
	abi = "Socket(((bytes4,uint64,uint128),uint8,(bytes4,bytes16),(bytes32,bytes32,address,address,uint256,bytes)))"
)]
/// The `Socket` event from the `SocketExternal` contract.
pub struct Socket {
	pub msg: SocketMessage,
}

#[derive(Clone, ethers::contract::EthAbiType, Debug, PartialEq, Eq, Hash)]
/// The event enums originated from the `SocketExternal` contract.
pub enum SocketEvents {
	Socket(Socket),
}

impl ethers::contract::EthLogDecode for SocketEvents {
	fn decode_log(log: &RawLog) -> Result<Self, ethers::abi::Error>
	where
		Self: Sized,
	{
		if let Ok(decoded) = Socket::decode_log(log) {
			return Ok(SocketEvents::Socket(decoded))
		}
		Err(ethers::abi::Error::InvalidData)
	}
}

#[async_trait::async_trait]
/// The client to interact with the `Socket` contract instance.
pub trait BridgeRelayBuilder {
	/// Builds the `poll()` function call data.
	fn build_poll_call_data(&self, msg: SocketMessage, sigs: Signatures) -> Bytes;

	/// Builds the `poll()` transaction request.
	async fn build_transaction(
		&self,
		msg: SocketMessage,
		is_inbound: bool,
		relay_tx_chain_id: u32,
	) -> TransactionRequest;

	/// Build the signatures required to request `poll()`.
	async fn build_signatures(&self, msg: SocketMessage, is_inbound: bool) -> Signatures;

	/// Encodes the given socket message to bytes.
	fn encode_socket_message(&self, msg: SocketMessage) -> Bytes;

	/// Signs the given socket message.
	async fn sign_socket_message(&self, msg: SocketMessage) -> Signature;

	/// Get the signatures of the given message.
	async fn get_signatures(&self, msg: SocketMessage) -> Signatures;
}

impl From<Signature> for Signatures {
	fn from(signature: Signature) -> Self {
		let r: [u8; 32] = signature.r.into();
		let s: [u8; 32] = signature.s.into();
		let v: [u8; 1] = [signature.v as u8];
		Signatures { r: vec![r], s: vec![s], v: Bytes::from(v) }
	}
}
