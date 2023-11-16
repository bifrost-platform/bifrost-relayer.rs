use std::{collections::BTreeMap, sync::Arc};

use br_primitives::eth::{ChainID, SocketVariants};
use ethers::{providers::JsonRpcClient, types::TransactionRequest};

use crate::eth::{EthClient, SocketRelayMetadata};

#[async_trait::async_trait]
pub trait Filter {
	async fn request_filter(&self, metadata: SocketRelayMetadata);

	async fn filter_executable(&self, metadata: SocketRelayMetadata);

	async fn filter_max_fee(&self);

	async fn filter_receiver_balance(&self);

	fn build_transaction_request(&self, variants: SocketVariants) -> TransactionRequest;
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
impl<T: JsonRpcClient> Filter for ExecutionFilter<T> {
	async fn request_filter(&self, metadata: SocketRelayMetadata) {}

	async fn filter_executable(&self, metadata: SocketRelayMetadata) {
		let transaction_request = self.build_transaction_request(metadata.variants);
		if let Some(target_client) = self.system_clients.get(&metadata.dst_chain_id) {}
	}

	async fn filter_max_fee(&self) {}

	async fn filter_receiver_balance(&self) {}

	fn build_transaction_request(&self, variants: SocketVariants) -> TransactionRequest {
		TransactionRequest::default().to(variants.sender).data(variants.data)
	}
}
