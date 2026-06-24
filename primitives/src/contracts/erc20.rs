use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	Erc20Contract,
	"../abi/abi.erc20.json"
);

use Erc20Contract::Erc20ContractInstance;

pub type Erc20Instance<F, P, N> = Erc20ContractInstance<Arc<FillProvider<F, P, N>>, N>;
