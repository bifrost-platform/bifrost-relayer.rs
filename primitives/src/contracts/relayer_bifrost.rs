use ethers::prelude::abigen;

abigen!(
	RelayerManagerBifrost,
	"../abi/abi.relayer.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
