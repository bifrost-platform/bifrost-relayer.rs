use ethers::{
	prelude::SignerMiddleware,
	providers::{JsonRpcClient, Middleware},
	signers::Signer,
	types::{transaction::eip2718::TypedTransaction, U256},
};
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::eth::{PollSubmit, Signatures, SocketExternal};

use super::{EthClient, SocketMessage, TxResult};

/// The essential task that sends asynchronous transactions.
pub struct TransactionManager<T> {
	/// The ethereum client for the connected chain.
	pub client: Arc<EthClient<T>>,
	/// The receiver connected to the event channel.
	pub receiver: UnboundedReceiver<SocketMessage>,
}

impl<T: JsonRpcClient> TransactionManager<T> {
	/// Instantiates a new `TransactionManager` instance.
	pub fn new(client: Arc<EthClient<T>>) -> (Self, UnboundedSender<SocketMessage>) {
		let (sender, receiver) = mpsc::unbounded_channel::<SocketMessage>();

		(Self { client, receiver }, sender)
	}

	/// Starts the transaction manager. Listens to every new consumed event message.
	pub async fn run(&mut self) {
		while let Some(msg) = self.receiver.recv().await {
			println!("msg -> {:?}", msg);

			self.request_send_transaction(msg).await.unwrap();
		}
	}

	async fn request_send_transaction(&mut self, msg: SocketMessage) -> TxResult {
		let middleware = Arc::new(SignerMiddleware::new(
			self.client.provider.clone(),
			self.client.wallet.signer.clone(),
		));

		// TODO: Need specific addresses for each chain
		let socket = SocketExternal::new(self.client.config.socket_address, middleware.clone());
		// let socket = SocketExternal::new(H160::default(), middleware.clone());

		let nonce = middleware
			.get_transaction_count(self.client.wallet.signer.address(), None)
			.await
			.unwrap();

		let (max_fee_per_gas, max_priority_fee_per_gas) =
			middleware.clone().estimate_eip1559_fees(None).await.unwrap();

		let poll_submit =
			PollSubmit { msg, sigs: Signatures::default(), option: ethers::types::U256::default() };
		// let tx = msg.gas(U256::from(1000000)).gas_price(max_fee_per_gas).nonce(nonce);
		let mut tx: TypedTransaction = socket.poll(poll_submit).tx;
		tx.set_gas(U256::from(1000000)).set_nonce(nonce);

		match tx {
			TypedTransaction::Eip1559(ref mut inner) => {
				inner.max_fee_per_gas = Some(max_fee_per_gas);
				inner.max_priority_fee_per_gas = Some(max_priority_fee_per_gas)
			},
			_ => {
				tx.set_gas_price(max_fee_per_gas);
			},
		};

		let res = middleware
			.send_transaction(tx, None)
			.await
			.expect("fail to sending tx")
			.await
			.unwrap();

		println!("transaction result : {:?}", res);

		Ok(())
	}
}

// #[cfg(test)]
// mod tests {
// 	use std::{sync::Arc, time::Duration};

// 	use ethers::{
// 		providers::{Http, Provider},
// 		types::Address,
// 	};
// 	use tokio::time::Instant;

// 	use crate::eth::{PollSubmit, Signatures, SocketExternal, SocketMessage};

// 	#[tokio::test]
// 	async fn test_parse_tx_from_event() {
// 		let eth_endpoint = "";

// 		let _decoded_log_dict = r#"
// 		{
// 			"rid": {
// 				"requested_chain": 5,
// 				"round": 2464,
// 				"sequence_number": 292
// 			},
// 			"request_status": 1,
// 			"instruction": {
// 				"dst_chain": 49088,
// 				"instruction": "0x03010104000000000000000000000000"
// 			},
// 			"params": {
// 				"asset1": "0x00000001ffffffff000000053a815eba66eabe966a6ae7e5df9652eca24e9c54",
// 				"asset2": "0x00000001000000010000bfc0ffffffffffffffffffffffffffffffffffffffff",
// 				"sender": "0xb04571fa24f3edd516807fe97a27ba3b1c6b589c",
// 				"receiver": "0xb04571fa24f3edd516807fe97a27ba3b1c6b589c",
// 				"amount": 749412003486191616,
// 				"variants": "0x00"
// 			}
// 		}"#;

// 		let now = Instant::now();

// 		// let socket_event = SocketEvent::from(decoded_log_dict).unwrap();
// 		// println!("{:?}", socket_event);

// 		let client = Arc::new(Provider::<Http>::try_from(eth_endpoint).unwrap());
// 		let contract_address =
// 			"0x4A31FfeAc276CC5e508cAC0568d932d398C4DD84".parse::<Address>().unwrap();
// 		let contract = SocketExternal::new(contract_address, client.clone());

// 		let poll_submit = PollSubmit {
// 			msg: SocketMessage::default(),
// 			sigs: Signatures::default(),
// 			option: ethers::types::U256::default(),
// 		};

// 		match contract.poll(poll_submit).call().await {
// 			Ok(result) => {
// 				println!("result : {result:?}")
// 			},
// 			Err(e) => {
// 				println!("error : {e:?}")
// 			},
// 		}

// 		match contract.get_owners().call().await {
// 			Ok(result) => {
// 				println!("owner : {result:?}")
// 			},
// 			Err(e) => {
// 				println!("error : {e:?}")
// 			},
// 		}

// 		tokio::time::sleep_until(now + Duration::from_millis(3000)).await;

// 		// let evm_manager = EVMTransactionManager::new(client);
// 		// let tx = evm_manager.build("contract_name", "method_name", socket_event);

// 		// assert_eq!(tx.data, Some());
// 	}

// 	// use std::fs;

// 	// use ethabi::Contract;
// 	// use hex;

// 	// fn test_decode_event_from_ethlog() {
// 	// 	// input data
// 	// 	let data =
// 	// "0xa9059cbb000000000000000000000000d18a52ae66778bf9ece5515115875a313d45f0e900000000000000000000000000000000000000000000000000000000007fde60"
// 	// ;

// 	// 	// remove hex prefix: 0x
// 	// 	let prefix_removed_data = data.trim_start_matches("0x");

// 	// 	// get method id, 4 bytes from data
// 	// 	let method_id = &prefix_removed_data[0..8];
// 	// 	println!("method_id={}", method_id);

// 	// 	// load abi
// 	// 	let contract =
// 	// 		Contract::load(fs::read("../configs/abi.socket.external.json").unwrap().as_slice())
// 	// 			.unwrap();
// 	// 	// get matched function
// 	// 	let function = contract
// 	// 		.functions()
// 	// 		.into_iter()
// 	// 		.find(|f| hex::encode(f.short_signature()) == method_id)
// 	// 		.unwrap();

// 	// 	// method id removed data
// 	// 	let method_removed_data = &prefix_removed_data[8..];
// 	// 	// using selected function decodes input data
// 	// 	let tokens = function
// 	// 		.decode_input(hex::decode(method_removed_data).unwrap().as_slice())
// 	// 		.unwrap();

// 	// 	println!("{:?}", tokens);
// 	// }
// }
