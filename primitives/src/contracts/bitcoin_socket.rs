use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	BitcoinSocketContract,
	"../abi/abi.socket.bitcoin.json"
);

use BitcoinSocketContract::BitcoinSocketContractInstance;

pub type BitcoinSocketInstance<F, P> =
	BitcoinSocketContractInstance<Arc<FillProvider<F, P, AnyNetwork>>, AnyNetwork>;
