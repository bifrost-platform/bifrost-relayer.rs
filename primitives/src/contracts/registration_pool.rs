use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RegistrationPoolContract,
	"../abi/abi.registration_pool.bifrost.json"
);

use RegistrationPoolContract::RegistrationPoolContractInstance;

pub type RegistrationPoolInstance<F, P, T> =
	RegistrationPoolContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork>;
