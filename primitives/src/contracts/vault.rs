use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	VaultContract,
	"../abi/abi.vault.merged.json"
);

use VaultContract::VaultContractInstance;

pub type VaultInstance<F, P, N> = VaultContractInstance<Arc<FillProvider<F, P, N>>, N>;
