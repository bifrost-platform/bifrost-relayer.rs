use ethers::{
	prelude::abigen,
	providers::{JsonRpcClient, Provider},
	types::H160,
};
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

use crate::Err;

pub struct EVMTransactionManager<T> {
	pub client: Arc<Provider<T>>,
	pub queue: Vec<SocketMessage>,
	pub socket: Socket<Provider<T>>,
}

abigen!(Socket, "../configs/abi.socket.external.json");

impl<T: JsonRpcClient> EVMTransactionManager<T> {
	pub fn new(client: Arc<Provider<T>>, contract_address: H160) -> Result<Self, Err> {
		let socket = Socket::new(contract_address, client.clone());

		Ok(Self { client, queue: vec![], socket })
	}

	pub async fn run(&mut self) {
		loop {
			let now = Instant::now();

			let socket_event = self.queue.pop();

			match socket_event {
				Some(e) => {
					let poll_submit = PollSubmit {
						msg: e,
						sigs: Signatures::default(),
						option: ethers::types::U256::default(),
					};

					match self.socket.poll(poll_submit).call().await {
						Ok(result) => {
							println!("result : {result:?}")
						},
						Err(e) => {
							println!("error : {e:?}")
						},
					}
				},
				None => {
					println!("No active events")
				},
			}

			tokio::time::sleep_until(now + Duration::from_millis(3000)).await;
		}
	}
}

// impl<T> TransactionManager for EVMTransactionManager<T> {
// 	fn build(
// 		&self,
// 		_contract_name: &str,
// 		_method_name: &str,
// 		socket_event: SocketEvent,
// 	) -> TransactionRequest {
// 		let t = Bytes::from(serde_json::to_vec(&socket_event).unwrap().to_vec());

// 		TransactionRequest::new().data(t).value(0)
// 	}
// 	fn send(&self) {}
// }

#[cfg(test)]
mod tests {
	use std::{sync::Arc, time::Duration};

	use ethers::{
		providers::{Http, Provider},
		types::Address,
	};
	use tokio::time::Instant;

	use crate::{EVMTransactionManager, PollSubmit, Signatures, Socket, SocketMessage};

	#[tokio::test]
	async fn test_parse_tx_from_event() {
		let eth_endpoint = "https://mainnet.infura.io/v3/e45599cfef884017af1d1d01c203ef30";

		let decoded_log_dict = r#"
		{
			"rid": {
				"requested_chain": 5,
				"round": 2464,
				"sequence_number": 292
			},
			"request_status": 1,
			"instruction": {
				"dst_chain": 49088,
				"instruction": "0x03010104000000000000000000000000"
			},
			"params": {
				"asset1": "0x00000001ffffffff000000053a815eba66eabe966a6ae7e5df9652eca24e9c54",
				"asset2": "0x00000001000000010000bfc0ffffffffffffffffffffffffffffffffffffffff",
				"sender": "0xb04571fa24f3edd516807fe97a27ba3b1c6b589c",
				"receiver": "0xb04571fa24f3edd516807fe97a27ba3b1c6b589c",
				"amount": 749412003486191616,
				"variants": "0x00"
			}
		}"#;

		let now = Instant::now();

		// let socket_event = SocketEvent::from(decoded_log_dict).unwrap();
		// println!("{:?}", socket_event);

		let client = Arc::new(Provider::<Http>::try_from(eth_endpoint).unwrap());
		let contract_address =
			"0x4A31FfeAc276CC5e508cAC0568d932d398C4DD84".parse::<Address>().unwrap();
		let contract = Socket::new(contract_address, client.clone());

		let poll_submit = PollSubmit {
			msg: SocketMessage::default(),
			sigs: Signatures::default(),
			option: ethers::types::U256::default(),
		};

		match contract.poll(poll_submit).call().await {
			Ok(result) => {
				println!("result : {result:?}")
			},
			Err(e) => {
				println!("error : {e:?}")
			},
		}

		match contract.get_owners().call().await {
			Ok(result) => {
				println!("owner : {result:?}")
			},
			Err(e) => {
				println!("error : {e:?}")
			},
		}

		tokio::time::sleep_until(now + Duration::from_millis(3000)).await;

		// let evm_manager = EVMTransactionManager::new(client);
		// let tx = evm_manager.build("contract_name", "method_name", socket_event);

		// assert_eq!(tx.data, Some());
	}

	// use std::fs;

	// use ethabi::Contract;
	// use hex;

	// fn test_decode_event_from_ethlog() {
	// 	// input data
	// 	let data =
	// "0xa9059cbb000000000000000000000000d18a52ae66778bf9ece5515115875a313d45f0e900000000000000000000000000000000000000000000000000000000007fde60"
	// ;

	// 	// remove hex prefix: 0x
	// 	let prefix_removed_data = data.trim_start_matches("0x");

	// 	// get method id, 4 bytes from data
	// 	let method_id = &prefix_removed_data[0..8];
	// 	println!("method_id={}", method_id);

	// 	// load abi
	// 	let contract =
	// 		Contract::load(fs::read("../configs/abi.socket.external.json").unwrap().as_slice())
	// 			.unwrap();
	// 	// get matched function
	// 	let function = contract
	// 		.functions()
	// 		.into_iter()
	// 		.find(|f| hex::encode(f.short_signature()) == method_id)
	// 		.unwrap();

	// 	// method id removed data
	// 	let method_removed_data = &prefix_removed_data[8..];
	// 	// using selected function decodes input data
	// 	let tokens = function
	// 		.decode_input(hex::decode(method_removed_data).unwrap().as_slice())
	// 		.unwrap();

	// 	println!("{:?}", tokens);
	// }
}
