use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	OracleContract,
	"../abi/abi.oracle.bifrost.json"
);

use OracleContract::OracleContractInstance;

pub type OracleInstance<F, P, N> = OracleContractInstance<Arc<FillProvider<F, P, N>>, N>;
