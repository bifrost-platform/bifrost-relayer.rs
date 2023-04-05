use ethers::prelude::abigen;

abigen!(
	Authority,
	"../abi/abi.authority.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
