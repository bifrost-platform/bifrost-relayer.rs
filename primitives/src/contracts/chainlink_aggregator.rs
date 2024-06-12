use ethers::prelude::abigen;

abigen!(
	ChainlinkContract,
	"../abi/abi.aggregatorv3.chainlink.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
