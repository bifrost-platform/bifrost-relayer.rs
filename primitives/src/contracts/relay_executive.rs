use alloy::sol;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RelayExecutiveContract,
	"../abi/abi.relay_executive.bifrost.json"
);
