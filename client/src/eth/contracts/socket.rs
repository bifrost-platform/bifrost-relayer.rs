use ethers::{
	abi::RawLog,
	prelude::abigen,
	types::{Bytes, U256},
};

abigen!(
	SocketExternal,
	"../abi/abi.socket.external.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

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
pub trait SocketClient {
	/// Builds the `poll()` function call data.
	fn build_poll_call_data(&self, msg: SocketMessage, sigs: Signatures) -> Bytes;

	/// Get the estimated gas amount to execute `poll()`.
	async fn get_estimated_gas_for_poll(&self, msg: SocketMessage, sigs: Signatures) -> U256;

	/// Get the signatures of the given message.
	async fn get_signatures(&self, msg: SocketMessage) -> Signatures;
}
