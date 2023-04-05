use ethers::prelude::abigen;

abigen!(
	RelayerManager,
	"../abi/abi.relayer.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
