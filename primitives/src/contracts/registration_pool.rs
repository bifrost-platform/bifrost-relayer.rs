use ethers::prelude::abigen;

abigen!(
	RegistrationPoolContract,
	"../abi/abi.registration_pool.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
