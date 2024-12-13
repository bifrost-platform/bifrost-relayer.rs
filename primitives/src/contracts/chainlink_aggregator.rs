use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	ChainlinkContract,
	"../abi/abi.aggregatorv3.chainlink.json"
);

use ChainlinkContract::ChainlinkContractInstance;

pub type ChainlinkInstance<F, P, T> =
	ChainlinkContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork>;
