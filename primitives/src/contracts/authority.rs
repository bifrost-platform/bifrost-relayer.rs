use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	AuthorityContract,
	"../abi/abi.authority.merged.json"
);

use AuthorityContract::AuthorityContractInstance;

pub type AuthorityInstance<F, P, T> =
	AuthorityContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork>;
