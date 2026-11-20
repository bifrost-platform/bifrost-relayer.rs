use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	BtcDepositRegistryContract,
	"../abi/abi.btc_deposit_registry.bifrost.json"
);

use BtcDepositRegistryContract::BtcDepositRegistryContractInstance;

pub type BtcDepositRegistryInstance<F, P, N> =
	BtcDepositRegistryContractInstance<Arc<FillProvider<F, P, N>>, N>;
