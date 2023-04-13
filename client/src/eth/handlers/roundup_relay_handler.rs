use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler, VSPPhase2Metadata,
	DEFAULT_RETRIES,
};
use async_trait::async_trait;
use cccp_primitives::{
	cli::RoundupHandlerUtilityConfig,
	authority_external::AuthorityExternal,
	socket_bifrost::{SerializedRoundUp, SocketBifrost, SocketBifrostEvents},
	socket_external::{RoundUpSubmit, Signatures, SocketExternal},
	sub_display_format, RoundupHandlerUtilType,
};
use ethers::{
	abi::{Detokenize, Tokenize},
	contract::EthLogDecode,
	prelude::{TransactionReceipt, H256},
	providers::{JsonRpcClient, Provider},
	types::{Address, Log, TransactionRequest, H160, U256},
};
use std::{str::FromStr, sync::Arc};
use tokio::sync::broadcast::Receiver;
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "roundup-handler";

pub struct RoundupUtility<T> {
	/// The event senders that sends messages to the event channel.
	pub event_sender: Arc<EventSender>,
	/// Socket contract on external chain.
	pub socket_external: SocketExternal<Provider<T>>,
	/// Relayer contracts on external chain.
	pub relayer_external: AuthorityExternal<Provider<T>>,
	/// External chain id.
	pub id: u32,
}

/// The essential task that handles `roundup relay` related events.
pub struct RoundupRelayHandler<T> {
	/// The block receiver that consumes new blocks from the block channel.
	pub block_receiver: Receiver<BlockMessage>,
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The `Socket` contract instance on bifrost network.
	pub socket_bifrost: SocketBifrost<Provider<T>>,
	/// Utils for relay transaction to external
	pub roundup_utils: Vec<RoundupUtility<T>>,
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
				"-[{}] ✨ Imported #{:?} ({}) with target transactions({:?})",
				sub_display_format(SUB_LOG_TARGET),
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
				continue
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
							"-[{}] RoundUp event emitted. However, the majority has not yet been met. ({:?})",
							sub_display_format(SUB_LOG_TARGET),
							receipt.transaction_hash,
						);
						continue
					},
				},
				Err(e) => {
					log::error!(
						target: &self.client.get_chain_name(),
						"-[{}] Error on decoding RoundUp event ({:?}):{:?}",
						sub_display_format(SUB_LOG_TARGET),
						receipt.transaction_hash,
						e,
					);
					continue
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
		external_clients: Vec<Arc<EthClient<T>>>,
		socket_bifrost: SocketBifrost<Provider<T>>,
		roundup_util_configs: Vec<RoundupHandlerUtilityConfig>,
	) -> Self {
		let roundup_signature = socket_bifrost.abi().event("RoundUp").unwrap().signature();
		let roundup_utils = event_senders
			.iter()
			.map(|sender| {
				let (socket_external, relayer_external) = roundup_util_configs.iter().fold(
					(None, None),
					|(socket_ext, relay_ext), config| {
						if config.chain_id == sender.id {
							match config.contract_type {
								RoundupHandlerUtilType::Socket => (
									Some(SocketExternal::new(
										H160::from_str(&config.contract).unwrap(),
										external_clients
											.iter()
											.find(|client| client.get_chain_id() == config.chain_id)
											.unwrap()
											.get_provider(),
									)),
									relay_ext,
								),
								RoundupHandlerUtilType::RelayManager => (
									socket_ext,
									Some(AuthorityExternal::new(
										H160::from_str(&config.contract).unwrap(),
										external_clients
											.iter()
											.find(|client| client.get_chain_id() == config.chain_id)
											.unwrap()
											.get_provider(),
									)),
								),
							}
						} else {
							(socket_ext, relay_ext)
						}
					},
				);

				let socket_external = socket_external.expect("socket_external must be initialized");
				let relayer_external =
					relayer_external.expect("relayer_external must be initialized");

				RoundupUtility {
					event_sender: sender.clone(),
					socket_external,
					relayer_external,
					id: sender.id,
				}
			})
			.collect();

		Self { block_receiver, client, socket_bifrost, roundup_utils, roundup_signature }
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
		let mut stream = tokio_stream::iter(&self.roundup_utils);
		while let Some(target_chain) = stream.next().await {
			// Check roundup submitted to target chain before.
			if roundup_submit.round >
				target_chain.relayer_external.latest_round().call().await.unwrap()
			{
				let transaction_request = self.build_transaction_request(
					target_chain.socket_external.clone(),
					roundup_submit.clone(),
				);

				target_chain
					.event_sender
					.sender
					.send(EventMessage::new(
						DEFAULT_RETRIES,
						transaction_request,
						EventMetadata::VSPPhase2(VSPPhase2Metadata::new(
							roundup_submit.round,
							target_chain.event_sender.id,
						)),
						true,
					))
					.unwrap();
			}
		}
	}
}
