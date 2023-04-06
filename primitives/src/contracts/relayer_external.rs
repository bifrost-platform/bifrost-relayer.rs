use ethers::prelude::abigen;

abigen!(
	RelayerManager,
	"../abi/abi.relayer.external.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
