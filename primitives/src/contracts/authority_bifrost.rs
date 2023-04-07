use ethers::prelude::abigen;

abigen!(
	AuthorityBifrost,
	"../abi/abi.authority.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
