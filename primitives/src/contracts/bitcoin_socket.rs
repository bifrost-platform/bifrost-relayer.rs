use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	BitcoinSocketContract,
	"../abi/abi.socket.bitcoin.json"
);

use BitcoinSocketContract::BitcoinSocketContractInstance;

pub type BitcoinSocketInstance<F, P, N> =
	BitcoinSocketContractInstance<Arc<FillProvider<F, P, N>>, N>;
