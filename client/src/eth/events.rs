use super::EthClient;

use std::str::FromStr;

use cccp_primitives::eth::SOCKET_EVENT_SIG;
// use tokio::time::{sleep, Duration};
use tokio_stream::StreamExt;
use web3::{
	ethabi::{ParamType, Token},
	types::{Bytes, TransactionReceipt, H160, H256, U256, U64},
	Transport,
};

#[derive(Debug)]
pub struct ReqId {
	pub src_chain: Bytes,
	pub round_id: U256,
	pub sequence: U256,
}

impl Default for ReqId {
	fn default() -> Self {
		Self { src_chain: Bytes::default(), round_id: U256::default(), sequence: U256::default() }
	}
}

impl ReqId {
	fn build(&mut self, src_chain: Bytes, round_id: U256, sequence: U256) {
		self.src_chain = src_chain;
		self.round_id = round_id;
		self.sequence = sequence;
	}
}

#[derive(Debug)]
pub struct Instructions {
	pub dst_chain: Bytes,
	pub method: Bytes,
}

impl Default for Instructions {
	fn default() -> Self {
		Self { dst_chain: Bytes::default(), method: Bytes::default() }
	}
}

impl Instructions {
	fn build(&mut self, dst_chain: Bytes, method: Bytes) {
		self.dst_chain = dst_chain;
		self.method = method;
	}
}

#[derive(Debug)]
pub struct Params {
	pub src_asset_idx: Bytes,
	pub dst_asset_idx: Bytes,
	pub sender: H160,
	pub receiver: H160,
	pub amount: U256,
	pub variants: Bytes,
}

impl Default for Params {
	fn default() -> Self {
		Self {
			src_asset_idx: Bytes::default(),
			dst_asset_idx: Bytes::default(),
			sender: H160::default(),
			receiver: H160::default(),
			amount: U256::default(),
			variants: Bytes::default(),
		}
	}
}

impl Params {
	fn build(
		&mut self,
		src_asset_idx: Bytes,
		dst_asset_idx: Bytes,
		sender: H160,
		receiver: H160,
		amount: U256,
		variants: Bytes,
	) {
		self.src_asset_idx = src_asset_idx;
		self.dst_asset_idx = dst_asset_idx;
		self.sender = sender;
		self.receiver = receiver;
		self.amount = amount;
		self.variants = variants;
	}
}

#[derive(Debug)]
/// The parsed CCCP socket event message.
pub struct SocketMessage {
	pub req_id: ReqId,
	pub status: U256,
	pub instructions: Instructions,
	pub params: Params,
}

impl SocketMessage {
	fn new() -> Self {
		Self {
			req_id: ReqId::default(),
			status: U256::default(),
			instructions: Instructions::default(),
			params: Params::default(),
		}
	}
}

/// The essential task that detects and parse CCCP-related events.
pub struct EventDetector<T: Transport> {
	/// The ethereum client for the connected chain.
	pub client: EthClient<T>,
}

impl<T: Transport> EventDetector<T> {
	/// Instantiates a new `EventDetector` instance.
	pub fn new(client: EthClient<T>) -> Self {
		Self { client }
	}

	/// Starts the event detector. Reads every new mined block of the connected chain and starts to
	/// detect and store CCCP-related transaction events.
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

	/// TODO: impl
	async fn process_confirmed_transaction(&mut self, receipt: TransactionReceipt) {
		if let Some(to) = receipt.to {
			if to == self.client.config.socket_address {
				receipt.logs.into_iter().for_each(|log| {
					if log.topics[0] == H256::from_str(SOCKET_EVENT_SIG).unwrap() {
						let decoded = Self::decode_socket_event(log.data.0.as_slice());
						let socket_message = Self::parse_socket_message(&decoded[0]);
						println!("socket message = {:?}", socket_message);
					}
				})
			}
		}
	}

	fn decode_socket_event(data: &[u8]) -> Vec<Token> {
		let types = [ParamType::Tuple(vec![
			ParamType::Tuple(vec![
				ParamType::FixedBytes(4),
				ParamType::Uint(64),
				ParamType::Uint(128),
			]),
			ParamType::Uint(8),
			ParamType::Tuple(vec![ParamType::FixedBytes(4), ParamType::FixedBytes(16)]),
			ParamType::Tuple(vec![
				ParamType::FixedBytes(32),
				ParamType::FixedBytes(32),
				ParamType::Address,
				ParamType::Address,
				ParamType::Uint(256),
				ParamType::Bytes,
			]),
		])];
		match web3::ethabi::decode(&types, data) {
			Ok(decoded) => decoded,
			Err(error) => panic!("panic -> {:?}", error),
		}
	}

	// TODO: refactor, remove unwrap, split method
	fn parse_socket_message(socket_message: &Token) -> SocketMessage {
		let mut socket_message_builder = SocketMessage::new();
		match socket_message {
			Token::Tuple(msg) => {
				match &msg[0] {
					Token::Tuple(req_id) => {
						let src_chain =
							Bytes::try_from(req_id[0].clone().into_fixed_bytes().unwrap()).unwrap();
						let round_id = req_id[1].clone().into_uint().unwrap();
						let sequence = req_id[2].clone().into_uint().unwrap();
						socket_message_builder.req_id.build(src_chain, round_id, sequence);
					},
					_ => panic!("panic 2"),
				}
				match &msg[1] {
					Token::Uint(status) => {
						socket_message_builder.status = status.into();
					},
					_ => panic!("panic 3"),
				}
				match &msg[2] {
					Token::Tuple(instruction) => {
						let dst_chain =
							Bytes::try_from(instruction[0].clone().into_fixed_bytes().unwrap())
								.unwrap();
						let method =
							Bytes::try_from(instruction[1].clone().into_fixed_bytes().unwrap())
								.unwrap();
						socket_message_builder.instructions.build(dst_chain, method);
					},
					_ => panic!("panic 3"),
				}
				match &msg[3] {
					Token::Tuple(params) => {
						let src_asset_idx =
							Bytes::try_from(params[0].clone().into_fixed_bytes().unwrap()).unwrap();
						let dst_asset_idx =
							Bytes::try_from(params[1].clone().into_fixed_bytes().unwrap()).unwrap();
						let sender = params[2].clone().into_address().unwrap().into();
						let receiver = params[3].clone().into_address().unwrap().into();
						let amount = params[4].clone().into_uint().unwrap();
						let variants =
							Bytes::try_from(params[5].clone().into_bytes().unwrap()).unwrap();
						socket_message_builder.params.build(
							src_asset_idx,
							dst_asset_idx,
							sender,
							receiver,
							amount,
							variants,
						);
					},
					_ => panic!("panic 4"),
				}
			},
			_ => panic!("panic 1"),
		}
		socket_message_builder
	}
}
