use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	SocketQueueContract,
	"../abi/abi.socket_queue.bifrost.json"
);

use SocketQueueContract::SocketQueueContractInstance;

pub type SocketQueueInstance<F, P, T> =
	SocketQueueContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork>;
