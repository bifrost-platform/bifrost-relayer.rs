use alloy::sol;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	ChainlinkContract,
	"../abi/abi.aggregatorv3.chainlink.json"
);
