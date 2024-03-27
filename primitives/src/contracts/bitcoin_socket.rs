use ethers::prelude::abigen;

abigen!(
	BitcoinSocketContract,
	"../abi/abi.socket.bitcoin.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
