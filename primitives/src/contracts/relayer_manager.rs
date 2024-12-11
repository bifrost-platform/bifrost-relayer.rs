use alloy::sol;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RelayerManagerContract,
	"../abi/abi.relayer.bifrost.json"
);
