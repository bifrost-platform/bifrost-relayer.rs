use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	RelayQueueContract,
	"../abi/abi.relay_queue.bifrost.json"
);

use RelayQueueContract::RelayQueueContractInstance;

pub type RelayQueueInstance<F, P, N> = RelayQueueContractInstance<Arc<FillProvider<F, P, N>>, N>;
