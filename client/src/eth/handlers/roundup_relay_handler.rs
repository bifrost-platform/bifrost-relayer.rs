use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler, VSPPhase2Metadata,
	DEFAULT_RETRIES,
};
use async_trait::async_trait;
use cccp_primitives::{
	authority_external::AuthorityExternal,
	cli::RoundupHandlerUtilityConfig,
	eth::{RecoveredSignature, RoundUpEventStatus},
	socket_bifrost::{SerializedRoundUp, SocketBifrost, SocketBifrostEvents},
	socket_external::{RoundUpSubmit, Signatures, SocketExternal},
	sub_display_format, RoundupHandlerUtilType,
};
use ethers::{
	abi::{encode, Detokenize, Token, Tokenize},
	contract::EthLogDecode,
	prelude::{TransactionReceipt, H256},
	providers::{JsonRpcClient, Provider},
	types::{Address, Bytes, Log, Signature, TransactionRequest, H160, U256},
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
	pub authority_external: AuthorityExternal<Provider<T>>,
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
				Ok(serialized_log) => match RoundUpEventStatus::from_u8(serialized_log.status) {
					RoundUpEventStatus::NextAuthorityCommitted => {
						let roundup_submit = self
							.build_roundup_submit(
								serialized_log.roundup.round,
								serialized_log.roundup.new_relayers,
							)
							.await;
						self.broadcast_roundup(roundup_submit).await;
					},
					RoundUpEventStatus::NextAuthorityRelayed => {
						log::info!(
							target: &self.client.get_chain_name(),
							"-[{}] 👤 RoundUp event emitted. However, the majority has not yet been met. ({:?})",
							sub_display_format(SUB_LOG_TARGET),
							receipt.transaction_hash,
						);
						continue
					},
				},
				Err(e) => {
					log::error!(
						target: &self.client.get_chain_name(),
						"-[{}] Error on decoding RoundUp event ({:?}):{}",
						sub_display_format(SUB_LOG_TARGET),
						receipt.transaction_hash,
						e.to_string(),
					);
					sentry::capture_error(&e);
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
				let (socket_external, authority_external) = roundup_util_configs.iter().fold(
					(None, None),
					|(socket_ext, authority_ext), config| {
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
									authority_ext,
								),
								RoundupHandlerUtilType::Authority => (
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
							(socket_ext, authority_ext)
						}
					},
				);

				let socket_external = socket_external.expect("socket_external must be initialized");
				let authority_external =
					authority_external.expect("authority_external must be initialized");

				RoundupUtility {
					event_sender: sender.clone(),
					socket_external,
					authority_external,
					id: sender.id,
				}
			})
			.collect();

		Self { block_receiver, client, socket_bifrost, roundup_utils, roundup_signature }
	}

	/// Decode & Serialize log to `SerializedRoundUp` struct.
	async fn decode_log(&self, log: Log) -> Result<SerializedRoundUp, ethers::abi::Error> {
		match SocketBifrostEvents::decode_log(&log.into()) {
			Ok(roundup) => Ok(SerializedRoundUp::from_tokens(roundup.into_tokens()).unwrap()),
			Err(error) => Err(error),
		}
	}

	/// Get the submitted signatures of the updated round.
	async fn get_sorted_signatures(&self, round: U256, new_relayers: Vec<Address>) -> Signatures {
		let encoded_msg = encode(&[
			Token::Uint(round),
			Token::Array(new_relayers.iter().map(|address| Token::Address(*address)).collect()),
		]);

		// looks unnecessary, but bifrost_socket::Signatures != external_socket::Signatures
		let unordered_sigs = Signatures::from_tokens(
			self.socket_bifrost
				.get_round_signatures(round)
				.call()
				.await
				.unwrap()
				.into_tokens(),
		)
		.unwrap_or_default();

		let unordered_concated_v = &unordered_sigs.v.to_string()[2..];

		// TODO: Maybe BTreeMap(key: signer, value: signature) could replace Vec<RecoveredSignature>
		let mut recovered_sigs = vec![];
		for idx in 0..unordered_sigs.r.len() {
			let sig = Signature {
				r: unordered_sigs.r[idx].into(),
				s: unordered_sigs.s[idx].into(),
				v: u64::from_str_radix(&unordered_concated_v[idx * 2..idx * 2 + 2], 16).unwrap(),
			};
			recovered_sigs.push(RecoveredSignature::new(
				idx,
				sig,
				self.client.wallet.recover_message(sig, &encoded_msg),
			));
		}
		recovered_sigs.sort_by_key(|k| k.signer);

		let mut sorted_sigs = Signatures::default();
		let mut sorted_concated_v = String::from("0x");
		recovered_sigs.into_iter().for_each(|sig| {
			let idx = sig.idx;
			sorted_sigs.r.push(unordered_sigs.r[idx]);
			sorted_sigs.s.push(unordered_sigs.s[idx]);
			let v = Bytes::from([sig.signature.v as u8]);
			sorted_concated_v.push_str(&v.to_string()[2..]);
		});
		sorted_sigs.v = Bytes::from_str(&sorted_concated_v).unwrap();

		sorted_sigs
	}

	/// Build `round_control_relay` method call param.
	async fn build_roundup_submit(
		&self,
		round: U256,
		mut new_relayers: Vec<Address>,
	) -> RoundUpSubmit {
		new_relayers.sort();
		let sigs = self.get_sorted_signatures(round, new_relayers.clone()).await;

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
				target_chain.authority_external.latest_round().call().await.unwrap()
			{
				let transaction_request = self.build_transaction_request(
					target_chain.socket_external.clone(),
					roundup_submit.clone(),
				);

				let metadata =
					VSPPhase2Metadata::new(roundup_submit.round, target_chain.event_sender.id);
				match target_chain.event_sender.sender.send(EventMessage::new(
					DEFAULT_RETRIES,
					transaction_request,
					EventMetadata::VSPPhase2(metadata.clone()),
					true,
				)) {
					Ok(()) => log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] 🔖 Request roundup transaction to chain({:?}): {}",
						sub_display_format(SUB_LOG_TARGET),
						target_chain.id,
						metadata,
					),
					Err(error) => {
						log::error!(
							target: &self.client.get_chain_name(),
							"-[{}] ❗️ Failed to request roundup transaction to chain({:?}): {}, Error: {}",
							sub_display_format(SUB_LOG_TARGET),
							target_chain.id,
							metadata,
							error.to_string()
						);
						sentry::capture_error(&error);
					},
				}
			}
		}
	}
}
