use ethers::prelude::abigen;

abigen!(
	UnifiedBtcContract,
	"../abi/abi.unified.erc20.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
