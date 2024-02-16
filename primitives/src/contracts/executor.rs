use ethers::prelude::abigen;

abigen!(
	ExecutorContract,
	"../abi/abi.executor.merged.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
