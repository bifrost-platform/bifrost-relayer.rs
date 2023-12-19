use std::{collections::BTreeMap, str::FromStr};

use ethers::{
	abi::RawLog,
	prelude::{abigen, H256},
	types::{Bytes, Signature, U256},
};

abigen!(
	SocketContract,
	"../abi/abi.socket.merged.json",
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
			return Ok(SocketEvents::Socket(decoded));
		}
		Err(ethers::abi::Error::InvalidData)
	}
}

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
	name = "RoundUp",
	abi = "RoundUp(uint8,(uint256,address[],(bytes32[],bytes32[],bytes)))"
)]
pub struct SerializedRoundUp {
	pub status: u8,
	pub roundup: RoundUpSubmit,
}

pub fn get_asset_oids() -> BTreeMap<String, H256> {
	BTreeMap::from([
		(
			"BFC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000001")
				.unwrap(),
		),
		(
			"BIFI".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000002")
				.unwrap(),
		),
		(
			"BTC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000003")
				.unwrap(),
		),
		(
			"ETH".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000004")
				.unwrap(),
		),
		(
			"BNB".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000005")
				.unwrap(),
		),
		(
			"MATIC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000006")
				.unwrap(),
		),
		(
			"AVAX".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000007")
				.unwrap(),
		),
		(
			"USDC".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000008")
				.unwrap(),
		),
		(
			"BUSD".to_string(),
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000009")
				.unwrap(),
		),
		(
			"USDT".to_string(),
			H256::from_str("010001000000000000000000000000000000000000000000000000000000000a")
				.unwrap(),
		),
		(
			"DAI".to_string(),
			H256::from_str("0x010001000000000000000000000000000000000000000000000000000000000b")
				.unwrap(),
		),
	])
}

impl From<Signature> for Signatures {
	fn from(signature: Signature) -> Self {
		let r: [u8; 32] = signature.r.into();
		let s: [u8; 32] = signature.s.into();
		let v: [u8; 1] = [signature.v as u8];
		Signatures { r: vec![r], s: vec![s], v: Bytes::from(v) }
	}
}
