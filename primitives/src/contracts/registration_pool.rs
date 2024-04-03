use ethers::{abi::RawLog, prelude::abigen, types::Address};

abigen!(
	RegistrationPoolContract,
	"../abi/abi.registration_pool.bifrost.json",
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
#[ethevent(name = "VaultPending", abi = "VaultPending(address, string)")]
pub struct VaultPending {
	pub user_bfc_address: Address,
	pub refund_address: String,
}

#[derive(Clone, ethers::contract::EthAbiType, Debug, PartialEq, Eq, Hash)]
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
