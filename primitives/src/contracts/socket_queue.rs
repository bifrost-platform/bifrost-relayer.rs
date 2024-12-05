use alloy::sol;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	SocketQueueContract,
	"../abi/abi.socket_queue.bifrost.json"
);
