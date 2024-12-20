use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RelayerManagerContract,
	"../abi/abi.relayer.bifrost.json"
);

use RelayerManagerContract::RelayerManagerContractInstance;

pub type RelayerManagerInstance<F, P, T> =
	RelayerManagerContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork>;
