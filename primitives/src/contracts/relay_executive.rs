use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RelayExecutiveContract,
	"../abi/abi.relay_executive.bifrost.json"
);

use RelayExecutiveContract::RelayExecutiveContractInstance;

pub type RelayExecutiveInstance<F, P, T> =
	RelayExecutiveContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork>;
