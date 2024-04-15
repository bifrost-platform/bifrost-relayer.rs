use ethers::prelude::abigen;

abigen!(
	RelayExecutiveContract,
	"../abi/abi.relay_executive.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
