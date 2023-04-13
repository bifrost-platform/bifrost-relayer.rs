use ethers::prelude::abigen;

abigen!(
	AuthorityExternal,
	"../abi/abi.authority.external.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
