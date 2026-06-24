use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug, PartialEq, Eq, Default)]
	#[sol(rpc)]
	OracleManagerContract,
	"../abi/abi.oracle.bifrost.json"
);

use OracleManagerContract::OracleManagerContractInstance;

pub type OracleManagerInstance<F, P, N> =
	OracleManagerContractInstance<Arc<FillProvider<F, P, N>>, N>;
