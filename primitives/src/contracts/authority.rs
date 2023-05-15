use ethers::prelude::abigen;

abigen!(
	AuthorityContract,
	"../abi/abi.authority.merged.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
