use ethers::prelude::abigen;

abigen!(
	RelayerManagerContract,
	"../abi/abi.relayer.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
