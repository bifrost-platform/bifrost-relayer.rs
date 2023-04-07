use ethers::prelude::abigen;

abigen!(
	RelayerManagerExternal,
	"../abi/abi.relayer.external.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
