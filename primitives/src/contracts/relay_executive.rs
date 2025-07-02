use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RelayExecutiveContract,
	"../abi/abi.relay_executive.bifrost.json"
);

use RelayExecutiveContract::RelayExecutiveContractInstance;

pub type RelayExecutiveInstance<F, P, N> =
	RelayExecutiveContractInstance<Arc<FillProvider<F, P, N>>, N>;
