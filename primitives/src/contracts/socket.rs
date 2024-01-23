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

pub fn get_asset_oids() -> BTreeMap<&'static str, H256> {
	<BTreeMap<&str, H256>>::from([
		(
			"BFC",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000001")
				.unwrap(),
		),
		(
			"BIFI",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000002")
				.unwrap(),
		),
		(
			"BTC",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000003")
				.unwrap(),
		),
		(
			"ETH",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000004")
				.unwrap(),
		),
		(
			"BNB",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000005")
				.unwrap(),
		),
		(
			"MATIC",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000006")
				.unwrap(),
		),
		(
			"AVAX",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000007")
				.unwrap(),
		),
		(
			"USDC",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000008")
				.unwrap(),
		),
		(
			"BUSD",
			H256::from_str("0100010000000000000000000000000000000000000000000000000000000009")
				.unwrap(),
		),
		(
			"USDT",
			H256::from_str("010001000000000000000000000000000000000000000000000000000000000a")
				.unwrap(),
		),
		(
			"DAI",
			H256::from_str("0x010001000000000000000000000000000000000000000000000000000000000b")
				.unwrap(),
		),
		(
			"BTCB",
			H256::from_str("0x010001000000000000000000000000000000000000000000000000000000000c")
				.unwrap(),
		),
		(
			"WBTC",
			H256::from_str("0x010001000000000000000000000000000000000000000000000000000000000d")
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
