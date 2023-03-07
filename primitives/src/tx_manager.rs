use ethers::{
	prelude::abigen,
	types::{Bytes, TransactionRequest},
};

use crate::{traits::TransactionManager, SocketEvent};

pub struct EVMTransactionManager;

abigen!(Socket, "../configs/abi.socket.external.json");

impl EVMTransactionManager {
	// pub fn encode_transaction_data(
	// 	&self,
	// 	contract_name: &str,
	// 	method_name: &str,
	// 	method_params: &str,
	// ) {
	// }

	pub fn set_contract(&self) {}
}

impl TransactionManager for EVMTransactionManager {
	fn build(
		&self,
		_contract_name: &str,
		_method_name: &str,
		socket_event: SocketEvent,
	) -> TransactionRequest {
		let t = Bytes::from(serde_json::to_vec(&socket_event).unwrap().to_vec());

		TransactionRequest::new().data(t).value(0)
	}
	fn send(&self) {}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use ethers::{
		providers::{Http, Provider},
		types::Address,
	};

	use crate::{traits::TransactionManager, EVMTransactionManager, Socket, SocketEvent};

	#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
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

		let socket_event = SocketEvent::from(decoded_log_dict).unwrap();
		println!("{:?}", socket_event);

		let client = Arc::new(Provider::<Http>::try_from(eth_endpoint).unwrap());
		let contract_address =
			"0x4A31FfeAc276CC5e508cAC0568d932d398C4DD84".parse::<Address>().unwrap();
		let contract = Socket::new(contract_address, client);

		let evm_manager = EVMTransactionManager;
		let tx = evm_manager.build("contract_name", "method_name", socket_event);

		// assert_eq!(tx.data, Some());
	}
}
