use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RelayerManagerContract,
	"../abi/abi.relayer.bifrost.json"
);

use RelayerManagerContract::RelayerManagerContractInstance;

pub type RelayerManagerInstance<F, P, N> =
	RelayerManagerContractInstance<Arc<FillProvider<F, P, N>>, N>;
