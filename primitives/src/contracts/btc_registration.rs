use ethers::{abi::RawLog, prelude::abigen, types::Address};

abigen!(
	RegisContract,
	"../abi/abi.registration.bifrost.json",
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
#[ethevent(name = "VaultPending", abi = "VaultPending(address[], string)")]
/// The `Socket` event from the `SocketExternal` contract.
pub struct VaultPending {
	pub user_bfc_address: Address,
	pub refund_address: String,
}

#[derive(Clone, ethers::contract::EthAbiType, Debug, PartialEq, Eq, Hash)]
/// The event enums originated from the `SocketExternal` contract.
pub enum BtcRegisEvents {
	Registration(VaultPending),
}

impl ethers::contract::EthLogDecode for BtcRegisEvents {
	fn decode_log(log: &RawLog) -> Result<Self, ethers::abi::Error>
	where
		Self: Sized,
	{
		if let Ok(decoded) = VaultPending::decode_log(log) {
			return Ok(BtcRegisEvents::Registration(decoded));
		}
		Err(ethers::abi::Error::InvalidData)
	}
}
