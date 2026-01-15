use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	HooksContract,
	"../abi/abi.hooks.json"
);

use HooksContract::HooksContractInstance;

pub type HooksInstance<F, P, N> = HooksContractInstance<Arc<FillProvider<F, P, N>>, N>;
