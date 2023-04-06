use ethers::prelude::{abigen, H256};
use std::{collections::HashMap, str::FromStr};

abigen!(
	SocketBifrost,
	"../abi/abi.socket.bifrost.json",
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
	name = "RoundUp",
	abi = "RoundUp(uint8,(uint256,address[],(bytes32[],bytes32[],bytes)))"
)]
pub struct SerializedRoundUp {
	pub status: u8,
	pub roundup: RoundUpSubmit,
}

pub fn get_asset_oids() -> HashMap<String, H256> {
	HashMap::from([
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
	])
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::eth::ROUNDUP_EVENT_SIG;
	use ethers::{
		abi::{Detokenize, Tokenize},
		contract::EthLogDecode,
		providers::{Http, Middleware, Provider},
		types::{H160, U256},
	};
	use std::sync::Arc;

	#[tokio::test]
	async fn test_events() {
		let provider = Provider::<Http>::try_from("").unwrap();
		let tx = provider
			.get_transaction_receipt(
				H256::from_str(
					"0x8e1ec970445343fe00ce6939b7475691e5c83d37cd7e448960799a996f3555ca",
				)
				.unwrap(),
			)
			.await
			.unwrap()
			.unwrap();

		let roundup_event_abi = SOCKETBIFROST_ABI.event("RoundUp").unwrap().clone();
		assert_eq!(H256::from_str(ROUNDUP_EVENT_SIG).unwrap(), roundup_event_abi.signature());

		match SocketBifrostEvents::decode_log(&tx.logs[0].clone().into()) {
			Ok(roundup) => match SerializedRoundUp::from_tokens(roundup.into_tokens()) {
				Ok(serialized_roundup) => {
					println!("{:?}", serialized_roundup);
				},
				Err(error) => {
					println!("Error on serializing roundup event: {:#?}", error)
				},
			},
			Err(error) => {
				println!("Invalid log data: {:#?}", error);
				return
			},
		}
	}

	#[tokio::test]
	async fn test_get_round_signatures() {
		let provider = Arc::new(Provider::<Http>::try_from("").unwrap());

		let socket_bifrost = SocketBifrost::new(
			H160::from_str("0xd551F33Ca8eCb0Be83d8799D9C68a368BA36Dd52").unwrap(),
			provider.clone(),
		);
		let sigs = socket_bifrost.get_round_signatures(U256::from(167)).call().await.unwrap();

		println!("{:?}", sigs);
	}
}