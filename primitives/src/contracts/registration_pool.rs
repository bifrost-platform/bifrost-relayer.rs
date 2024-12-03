use alloy::sol;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RegistrationPoolContract,
	"../abi/abi.registration_pool.bifrost.json"
);
