use ethers::prelude::abigen;

abigen!(
	SocketQueueContract,
	"../abi/abi.socket_queue.bifrost.json",
	event_derives(serde::Deserialize, serde::Serialize)
);
