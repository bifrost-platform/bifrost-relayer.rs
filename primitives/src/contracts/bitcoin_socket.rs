use alloy::sol;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	BitcoinSocketContract,
	"../abi/abi.socket.bitcoin.json"
);
