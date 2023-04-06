use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler, VSPPhase2Metadata,
	DEFAULT_RETRIES,
};
use async_trait::async_trait;
use cccp_primitives::{
	relayer_external::RelayerManager,
	socket_bifrost::{SerializedRoundUp, SocketBifrost, SocketBifrostEvents, SOCKETBIFROST_ABI},
	socket_external::{RoundUpSubmit, Signatures, SocketExternal},
};
use ethers::{
	abi::{Detokenize, Tokenize},
	contract::EthLogDecode,
	prelude::{TransactionReceipt, H256},
	providers::{JsonRpcClient, Provider},
	types::{Address, Log, TransactionRequest, U256},
};
use std::sync::Arc;
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

pub struct SocketExternalInstance<T> {
	pub contract: SocketExternal<Provider<T>>,
	pub id: u32,
}

pub struct RelayerExternalInstance<T> {
	pub contract: RelayerManager<Provider<T>>,
	pub id: u32,
}

/// The essential task that handles `roundup relay` related events.
pub struct RoundupRelayHandler<T> {
	/// The event senders that sends messages to the event channel.
	pub event_senders: Vec<Arc<EventSender>>,
	/// The block receiver that consumes new blocks from the block channel.
	pub block_receiver: Receiver<BlockMessage>,
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The target `Socket` contract instance.
	pub socket_bifrost: SocketBifrost<Provider<T>>,
	/// External socket contracts.
	pub socket_externals: Vec<SocketExternalInstance<T>>,
	/// Relayer contracts on external chains.
	pub relayer_externals: Vec<RelayerExternalInstance<T>>,
	/// Signature of RoundUp Event.
	pub roundup_signature: H256,
}

#[async_trait]
impl<T: JsonRpcClient> Handler for RoundupRelayHandler<T> {
	async fn run(&mut self) {
		loop {
			let block_msg = self.block_receiver.recv().await.unwrap();

			log::info!(
				target: &self.client.get_chain_name(),
				"-[roundup-handler    ] ✨ Imported #{:?} ({}) with target transactions({:?})",
				block_msg.raw_block.number.unwrap(),
				block_msg.raw_block.hash.unwrap(),
				block_msg.target_receipts.len(),
			);

			let mut stream = tokio_stream::iter(block_msg.target_receipts);
			while let Some(receipt) = stream.next().await {
				self.process_confirmed_transaction(receipt).await;
			}
		}
	}

	async fn process_confirmed_transaction(&self, receipt: TransactionReceipt) {
		// Pass if interacted contract was not socket contract
		if !self.is_target_contract(&receipt) {
			return
		}

		let mut stream = tokio_stream::iter(receipt.logs);
		while let Some(log) = stream.next().await {
			// Pass if emitted event is not `RoundUp`
			if !self.is_target_event(log.topics[0]) {
				break
			}

			match self.decode_log(log).await {
				Ok(serialized_log) => match serialized_log.status {
					10 => {
						let roundup_submit = self
							.build_roundup_submit(
								serialized_log.roundup.round,
								serialized_log.roundup.new_relayers,
							)
							.await;
						self.broadcast_roundup(roundup_submit).await;
					},
					_ => {
						log::info!(
							target: &self.client.get_chain_name(),
							"-[roundup-handler    ] RoundUp event emitted. However, the majority has not yet been met. ({:?})",
							receipt.transaction_hash,
						);
						break
					},
				},
				Err(e) => {
					log::error!(
						target: &self.client.get_chain_name(),
						"-[roundup-handler    ] Error on decoding RoundUp event ({:?}):{:?}",
						receipt.transaction_hash,
						e,
					);
					break
				},
			}
		}
	}

	fn is_target_contract(&self, receipt: &TransactionReceipt) -> bool {
		if let Some(to) = receipt.to {
			return ethers::utils::to_checksum(&to, None) ==
				ethers::utils::to_checksum(&self.socket_bifrost.address(), None)
		}
		false
	}

	fn is_target_event(&self, topic: H256) -> bool {
		topic == self.roundup_signature
	}
}

impl<T: JsonRpcClient> RoundupRelayHandler<T> {
	/// Instantiates a new `RoundupRelayHandler` instance.
	pub fn new(
		event_senders: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		client: Arc<EthClient<T>>,
		socket_bifrost: SocketBifrost<Provider<T>>,
		socket_externals: Vec<SocketExternalInstance<T>>,
		relayer_externals: Vec<RelayerExternalInstance<T>>,
	) -> Self {
		Self {
			event_senders,
			block_receiver,
			client: client.clone(),
			socket_bifrost,
			socket_externals,
			relayer_externals,
			roundup_signature: SOCKETBIFROST_ABI.event("RoundUp").unwrap().signature(),
		}
	}

	/// Decode & Serialize log to `SerializedRoundUp` struct.
	async fn decode_log(&self, log: Log) -> Result<SerializedRoundUp, ethers::abi::Error> {
		return match SocketBifrostEvents::decode_log(&log.clone().into()) {
			Ok(roundup) => Ok(SerializedRoundUp::from_tokens(roundup.into_tokens()).unwrap()),
			Err(error) => Err(error),
		}
	}

	/// Build `round_control_relay` method call param.
	async fn build_roundup_submit(
		&self,
		round: U256,
		mut new_relayers: Vec<Address>,
	) -> RoundUpSubmit {
		new_relayers.sort();

		let bifrost_sigs = self.socket_bifrost.get_round_signatures(round).call().await.unwrap();
		let sigs = Signatures::from_tokens(bifrost_sigs.into_tokens()).unwrap();

		RoundUpSubmit { round, new_relayers, sigs }
	}

	/// Build `round_control_relay` method call transaction.
	fn build_transaction_request(
		&self,
		target_contract: SocketExternal<Provider<T>>,
		roundup_submit: RoundUpSubmit,
	) -> TransactionRequest {
		TransactionRequest::default()
			.to(target_contract.address())
			.data(target_contract.round_control_relay(roundup_submit).calldata().unwrap())
	}

	/// Check roundup submitted before. If not, call `round_control_relay`.
	async fn broadcast_roundup(&self, roundup_submit: RoundUpSubmit) {
		let mut stream = tokio_stream::iter(self.event_senders.clone());
		while let Some(sender) = stream.next().await {
			let relayer_manager = self
				.relayer_externals
				.iter()
				.find(|relayer_manager| relayer_manager.id == sender.id)
				.unwrap();
			let target_round = relayer_manager.contract.latest_round().call().await.unwrap();

			// Check roundup submitted to target chain before.
			if roundup_submit.round > target_round {
				let target_socket =
					self.socket_externals.iter().find(|socket| socket.id == sender.id).unwrap();
				let transaction_request = self.build_transaction_request(
					target_socket.contract.clone(),
					roundup_submit.clone(),
				);

				self.request_send_transaction(
					sender.id,
					transaction_request,
					VSPPhase2Metadata::new(roundup_submit.round, sender.id),
				)
			}
		}
	}

	/// Request send bridge relay transaction to the target event channel.
	fn request_send_transaction(
		&self,
		chain_id: u32,
		tx_request: TransactionRequest,
		metadata: VSPPhase2Metadata,
	) {
		if let Some(event_sender) =
			self.event_senders.iter().find(|event_sender| event_sender.id == chain_id)
		{
			event_sender
				.sender
				.send(EventMessage::new(
					DEFAULT_RETRIES,
					tx_request,
					EventMetadata::VSPPhase2(metadata),
				))
				.unwrap();
		} else {
			panic!(
				"{}]-[Roundup-handler    ] Unknown chain ID received: {:?}",
				self.client.get_chain_name(),
				chain_id
			)
		}
	}
}
