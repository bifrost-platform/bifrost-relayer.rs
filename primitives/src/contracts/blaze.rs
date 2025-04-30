use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	BlazeContract,
	"../abi/abi.blaze.bifrost.json"
);

use BlazeContract::BlazeContractInstance;

pub type BlazeInstance<F, P> =
	BlazeContractInstance<(), Arc<FillProvider<F, P, AnyNetwork>>, AnyNetwork>;
