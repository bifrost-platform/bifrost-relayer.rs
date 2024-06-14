use ethers::prelude::abigen;

abigen!(
	VaultContract,
	"../abi/abi.vault.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
