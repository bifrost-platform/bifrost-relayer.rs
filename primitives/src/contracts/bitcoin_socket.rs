use ethers::prelude::abigen;

abigen!(
	BitcoinSocketContract,
	"../abi/abi.socket.bitcoin.json",
	event_derives(serde::Deserialize, serde::Serialize)
);

#[derive(Debug, Clone, PartialEq, ethers::contract::EthEvent, ethers::contract::EthDisplay)]
#[ethevent(
	name = "Socket",
	abi = "Socket(((bytes4,uint64,uint128),uint8,(bytes4,bytes16),(bytes32,bytes32,address,address,uint256,bytes)))"
)]
pub struct Socket {
	pub msg: SocketMessage,
}
