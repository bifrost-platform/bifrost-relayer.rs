use super::{EthClient, Instruction, RequestID, SocketMessage, TaskParams};

use std::str::FromStr;

use cccp_primitives::eth::SOCKET_EVENT_SIG;
use ethers::{
	abi::{
		ParamType::{Address, Bytes, FixedBytes, Tuple, Uint},
		Token, Tokenizable,
	},
	providers::JsonRpcClient,
	types::{TransactionReceipt, H256, U64},
};
// use tokio::time::{sleep, Duration};
use tokio_stream::StreamExt;

/// The essential task that detects and parse CCCP-related events.
pub struct EventDetector<T> {
	/// The ethereum client for the connected chain.
	pub client: EthClient<T>,
}

impl<T: JsonRpcClient> EventDetector<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: EthClient<T>) -> Self {
		Self { client }
	}

	/// Starts the event detector. Reads every new mined block of the connected chain and starts to
	/// detect and store `Socket` transaction events.
	pub async fn run(&mut self) {
		// TODO: follow-up to the highest block
		// loop {
		// 	let latest_block = self.client.get_latest_block_number().await.unwrap();
		// 	if let Some(confirmed_block) = self.client.try_push_block(latest_block) {
		// 		self.process_confirmed_block(confirmed_block).await;
		// 	}

		// 	sleep(Duration::from_millis(self.client.config.call_interval)).await;
		// 	println!("block number: {:?}", latest_block);
		// }

		// for test
		self.process_confirmed_block(1704591.into()).await;
	}

	/// Reads the contained transactions of the given confirmed block. This method will stream
	/// through the transaction array and retrieve its data.
	async fn process_confirmed_block(&mut self, block: U64) {
		if let Some(block) = self.client.get_block(block.into()).await.unwrap() {
			let mut stream = tokio_stream::iter(block.transactions);

			while let Some(tx) = stream.next().await {
				if let Some(receipt) = self.client.get_transaction_receipt(tx).await.unwrap() {
					self.process_confirmed_transaction(receipt).await;
				}
			}
		}
	}

	/// Decode and parse the socket event if the given transaction triggered an event.
	async fn process_confirmed_transaction(&mut self, receipt: TransactionReceipt) {
		if Self::is_socket_contract(&self, &receipt) {
			receipt.logs.iter().for_each(|log| {
				if log.topics[0] == H256::from_str(SOCKET_EVENT_SIG).unwrap() {
					let decoded_socket_event = Self::decode_socket_event(&log.data);
					let socket_message = Self::parse_socket_message(&decoded_socket_event[0]);
					println!("socket message = {:?}", socket_message);
					self.client.push_event(socket_message);
				}
			})
		}
	}

	/// Verifies whether the given transaction interacted with the socket contract.
	fn is_socket_contract(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			if to == self.client.config.socket_address {
				return true
			}
		}
		false
	}

	/// Decode the raw event data of the triggered socket event.
	fn decode_socket_event(data: &[u8]) -> Vec<Token> {
		let abi_types = [Tuple(vec![
			// req_id(bytes4,uint64,uint128)
			Tuple(vec![FixedBytes(4), Uint(64), Uint(128)]),
			// status(uint8)
			Uint(8),
			// ins_code(bytes4,bytes16)
			Tuple(vec![FixedBytes(4), FixedBytes(16)]),
			// task_params(bytes32,bytes32,address,address,uint256,bytes)
			Tuple(vec![FixedBytes(32), FixedBytes(32), Address, Address, Uint(256), Bytes]),
		])];
		match ethers::abi::decode(&abi_types, data) {
			Ok(decoded) => decoded,
			Err(error) => panic!("panic -> {:?}", error),
		}
	}

	// TODO: refactor, remove unwrap, split method
	fn parse_socket_message(socket_message: &Token) -> SocketMessage {
		match socket_message {
			Token::Tuple(msg) => {
				let req_id = match &msg[0] {
					Token::Tuple(req_id) => {
						let chain =
							req_id[0].clone().into_fixed_bytes().unwrap().try_into().unwrap();
						let round_id = req_id[1].clone().into_uint().unwrap().try_into().unwrap();
						let sequence = req_id[2].clone().into_uint().unwrap().try_into().unwrap();
						RequestID { chain, round_id, sequence }
					},
					_ => panic!("panic 2"),
				};

				let status = match &msg[1] {
					Token::Uint(status) =>
						status.into_token().into_uint().unwrap().try_into().unwrap(),
					_ => panic!("panic 3"),
				};

				let ins_code = match &msg[2] {
					Token::Tuple(instruction) => {
						let chain =
							instruction[0].clone().into_fixed_bytes().unwrap().try_into().unwrap();
						let method =
							instruction[1].clone().into_fixed_bytes().unwrap().try_into().unwrap();
						Instruction { chain, method }
					},
					_ => panic!("panic 3"),
				};

				let params = match &msg[3] {
					Token::Tuple(params) => {
						let token_idx0 =
							params[0].clone().into_fixed_bytes().unwrap().try_into().unwrap();
						let token_idx1 =
							params[1].clone().into_fixed_bytes().unwrap().try_into().unwrap();
						let refund = params[2].clone().into_address().unwrap().into();
						let to = params[3].clone().into_address().unwrap().into();
						let amount = params[4].clone().into_uint().unwrap();
						let variants = params[5].clone().into_bytes().unwrap().try_into().unwrap();
						TaskParams { token_idx0, token_idx1, refund, to, amount, variants }
					},
					_ => panic!("panic 4"),
				};

				SocketMessage { req_id, status, ins_code, params }
			},
			_ => panic!("panic 1"),
		}
	}
}
