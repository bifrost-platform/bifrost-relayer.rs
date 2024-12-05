use alloy::sol;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	AuthorityContract,
	"../abi/abi.authority.merged.json"
);
