use std::{collections::BTreeMap, sync::Arc};

use br_primitives::eth::ChainID;
use ethers::{
	providers::{JsonRpcClient, Middleware},
	types::{Address, Bytes, TransactionRequest, U256},
};

use crate::eth::{EthClient, LegacyGasMiddleware, SocketRelayMetadata, TxRequest};

#[async_trait::async_trait]
pub trait Filter<T> {
	async fn request(&self, metadata: SocketRelayMetadata);

	async fn filter_executable(
		&self,
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
	) -> U256;

	async fn filter_max_fee(
		&self,
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
		gas: U256,
	) -> U256;

	async fn filter_receiver_balance(
		&self,
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
		fee: U256,
	);

	fn build_transaction_request(&self, receiver: Address, data: Bytes) -> TransactionRequest;
}

pub struct ExecutionFilter<T> {
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	system_clients: BTreeMap<ChainID, Arc<EthClient<T>>>,
}

impl<T: JsonRpcClient> ExecutionFilter<T> {
	pub fn new(system_clients_vec: Vec<Arc<EthClient<T>>>) -> Self {
		let mut system_clients = BTreeMap::new();
		system_clients_vec.iter().for_each(|client| {
			system_clients.insert(client.get_chain_id(), client.clone());
		});

		Self { system_clients }
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> Filter<T> for ExecutionFilter<T> {
	async fn request(&self, metadata: SocketRelayMetadata) {
		if let Some(client) = self.system_clients.get(&metadata.dst_chain_id) {
			let gas = self.filter_executable(client, metadata.clone()).await;
			let fee = self.filter_max_fee(client, metadata.clone(), gas).await;
			self.filter_receiver_balance(client, metadata, fee).await;
		}
	}

	async fn filter_executable(
		&self,
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
	) -> U256 {
		let mut tx_request =
			self.build_transaction_request(metadata.receiver, metadata.variants.data);
		tx_request = tx_request.from(client.protocol_contracts.router_address);

		let estimated_gas = match client
			.provider
			.estimate_gas(&TxRequest::Legacy(tx_request).to_typed(), None)
			.await
		{
			Ok(estimated_gas) => estimated_gas,
			Err(error) => {
				// TODO: handle execution failure. add some retries
				panic!("");
			},
		};
		estimated_gas
	}

	async fn filter_max_fee(
		&self,
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
		gas: U256,
	) -> U256 {
		let gas_price = client.get_gas_price().await;
		let fee = gas.saturating_mul(gas_price);
		if fee > metadata.variants.max_fee {
			// TODO: handle fee exceed
			panic!("");
		}
		fee
	}

	async fn filter_receiver_balance(
		&self,
		client: &Arc<EthClient<T>>,
		metadata: SocketRelayMetadata,
		fee: U256,
	) {
		let balance = client.get_balance(metadata.receiver).await;
		if fee > balance {
			// TODO: handle insufficient funds
			panic!("");
		}
	}

	fn build_transaction_request(&self, receiver: Address, data: Bytes) -> TransactionRequest {
		TransactionRequest::default().to(receiver).data(data)
	}
}
