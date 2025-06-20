use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RegistrationPoolContract,
	"../abi/abi.registration_pool.bifrost.json"
);

use RegistrationPoolContract::RegistrationPoolContractInstance;

pub type RegistrationPoolInstance<F, P> =
	RegistrationPoolContractInstance<Arc<FillProvider<F, P, AnyNetwork>>, AnyNetwork>;
