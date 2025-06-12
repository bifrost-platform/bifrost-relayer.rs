use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	AuthorityContract,
	"../abi/abi.authority.merged.json"
);

use AuthorityContract::AuthorityContractInstance;

pub type AuthorityInstance<F, P> =
	AuthorityContractInstance<(), Arc<FillProvider<F, P, AnyNetwork>>, AnyNetwork>;
