use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use async_trait::async_trait;
use ethers::{
	abi::{encode, Detokenize, Token, Tokenize},
	contract::EthLogDecode,
	providers::{JsonRpcClient, Provider},
	types::{Address, Bytes, Filter, Log, Signature, TransactionRequest, H256, U256},
};
use tokio::{
	sync::{broadcast::Receiver, Barrier, Mutex, RwLock},
	time::sleep,
};
use tokio_stream::StreamExt;

use br_primitives::{
	authority::RoundMetaData,
	cli::{BootstrapConfig, BOOTSTRAP_DEFAULT_ROUND_OFFSET},
	eth::{
		BootstrapState, ChainID, GasCoefficient, RecoveredSignature, RoundUpEventStatus,
		BOOTSTRAP_BLOCK_CHUNK_SIZE,
	},
	socket::{RoundUpSubmit, SerializedRoundUp, Signatures, SocketContract, SocketContractEvents},
	sub_display_format, INVALID_BIFROST_NATIVENESS, INVALID_CONTRACT_ABI,
};

use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler, TxRequest,
	VSPPhase2Metadata,
};

use super::BootstrapHandler;

const SUB_LOG_TARGET: &str = "roundup-handler";

/// The essential task that handles `roundup relay` related events.
pub struct RoundupRelayHandler<T> {
	/// The event senders that sends messages to each event channel.
	event_senders: BTreeMap<ChainID, Arc<EventSender>>,
	/// The block receiver that consumes new blocks from the block channel.
	block_receiver: Receiver<BlockMessage>,
	/// The `EthClient` to interact with the bifrost network.
	client: Arc<EthClient<T>>,
	/// `EthClient`s to interact with provided networks except bifrost network.
	external_clients: Vec<Arc<EthClient<T>>>,
	/// Signature of RoundUp Event.
	roundup_signature: H256,
	/// Barrier for bootstrapping
	pub socket_barrier: Arc<Barrier>,
	/// Barrier for bootstrapping
	pub roundup_barrier: Arc<Barrier>,
	/// Completion of bootstrapping
	pub bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
	/// Completion of bootstrapping count
	pub bootstrapping_count: Arc<Mutex<u8>>,
	/// Bootstrap config
	pub bootstrap_config: Option<BootstrapConfig>,
}

#[async_trait]
impl<T: JsonRpcClient> Handler for RoundupRelayHandler<T> {
	async fn run(&mut self) {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapRoundUpPhase2).await {
				self.bootstrap().await;

				sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let block_msg = self.block_receiver.recv().await.unwrap();

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 📦 Imported #{:?} with target logs({:?})",
					sub_display_format(SUB_LOG_TARGET),
					block_msg.block_number,
					block_msg.target_logs.len(),
				);

				let mut stream = tokio_stream::iter(block_msg.target_logs);
				while let Some(log) = stream.next().await {
					self.process_confirmed_log(&log, false).await;
				}
			}
		}
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) {
		// Pass if interacted contract was not socket contract || not roundup event
		if !self.is_target_contract(log) || !self.is_target_event(log.topics[0]) {
			return
		}

		// Check receipt status
		if let Some(receipt) =
			self.client.get_transaction_receipt(log.transaction_hash.unwrap()).await
		{
			if receipt.status.unwrap().is_zero() {
				return
			}
		} else {
			return
		}

		match self.decode_log(log.clone()).await {
			Ok(serialized_log) => {
				if !is_bootstrap {
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] 👤 RoundUp event detected. ({:?}-{:?})",
						sub_display_format(SUB_LOG_TARGET),
						serialized_log.status,
						log.transaction_hash,
					);
				}
				match RoundUpEventStatus::from_u8(serialized_log.status) {
					RoundUpEventStatus::NextAuthorityCommitted => {
						if !self.is_selected_relayer(serialized_log.roundup.round - 1).await {
							// do nothing if not verified
							return
						}

						let roundup_submit = self
							.build_roundup_submit(
								serialized_log.roundup.round,
								serialized_log.roundup.new_relayers,
							)
							.await;
						self.broadcast_roundup(&roundup_submit).await;
					},
					RoundUpEventStatus::NextAuthorityRelayed => return,
				}
			},
			Err(e) => {
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] Error on decoding RoundUp event ({:?}):{}",
					sub_display_format(SUB_LOG_TARGET),
					log.transaction_hash,
					e.to_string(),
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] Error on decoding RoundUp event ({:?}):{}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						log.transaction_hash,
						e
					)
					.as_str(),
					sentry::Level::Error,
				);
				return
			},
		}
	}

	fn is_target_contract(&self, log: &Log) -> bool {
		log.address == self.client.contracts.socket.address()
	}

	fn is_target_event(&self, topic: H256) -> bool {
		topic == self.roundup_signature
	}
}

impl<T: JsonRpcClient> RoundupRelayHandler<T> {
	/// Instantiates a new `RoundupRelayHandler` instance.
	pub fn new(
		mut event_senders_vec: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		clients: Vec<Arc<EthClient<T>>>,
		socket_barrier: Arc<Barrier>,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
		bootstrap_config: Option<BootstrapConfig>,
		number_of_relay_targets: usize,
	) -> Self {
		// Only broadcast to external chains
		event_senders_vec.retain(|channel| !channel.is_native);

		let mut event_senders = BTreeMap::new();
		event_senders_vec.iter().for_each(|event_sender| {
			event_senders.insert(event_sender.id, event_sender.clone());
		});

		let client = clients
			.iter()
			.find(|client| client.metadata.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone();

		let external_clients =
			clients.into_iter().filter(|client| !client.metadata.is_native).collect();

		let roundup_signature = client
			.contracts
			.socket
			.abi()
			.event("RoundUp")
			.expect(INVALID_CONTRACT_ABI)
			.signature();

		let roundup_barrier = Arc::new(Barrier::new(number_of_relay_targets));
		let bootstrapping_count = Arc::new(Mutex::new(u8::default()));

		Self {
			event_senders,
			block_receiver,
			client,
			external_clients,
			roundup_signature,
			socket_barrier,
			roundup_barrier,
			bootstrap_states,
			bootstrapping_count,
			bootstrap_config,
		}
	}

	/// Decode & Serialize log to `SerializedRoundUp` struct.
	async fn decode_log(&self, log: Log) -> Result<SerializedRoundUp, ethers::abi::Error> {
		match SocketContractEvents::decode_log(&log.into()) {
			Ok(roundup) => Ok(SerializedRoundUp::from_tokens(roundup.into_tokens()).unwrap()),
			Err(error) => Err(error),
		}
	}

	/// Get the submitted signatures of the updated round.
	async fn get_sorted_signatures(&self, round: U256, new_relayers: &[Address]) -> Signatures {
		let encoded_msg = encode(&[
			Token::Uint(round),
			Token::Array(new_relayers.iter().map(|address| Token::Address(*address)).collect()),
		]);

		let unordered_sigs = self
			.client
			.contract_call(
				self.client.contracts.socket.get_round_signatures(round),
				"socket.get_round_signatures",
			)
			.await;
		let unordered_concated_v = &unordered_sigs.v.to_string()[2..];

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

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self, round: U256) -> bool {
		let relayer_manager = self.client.contracts.relayer_manager.as_ref().unwrap();
		self.client
			.contract_call(
				relayer_manager.is_previous_selected_relayer(round, self.client.address(), true),
				"relayer_manager.is_previous_selected_relayer",
			)
			.await
	}

	/// Verifies whether the bootstrap state has been synced to the given state.
	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_states.read().await.iter().all(|s| *s == state)
	}

	/// Build `round_control_relay` method call param.
	async fn build_roundup_submit(
		&self,
		round: U256,
		mut new_relayers: Vec<Address>,
	) -> RoundUpSubmit {
		new_relayers.sort();
		let sigs = self.get_sorted_signatures(round, &new_relayers).await;

		RoundUpSubmit { round, new_relayers, sigs }
	}

	/// Build `round_control_relay` method call transaction.
	fn build_transaction_request(
		&self,
		target_socket: &SocketContract<Provider<T>>,
		roundup_submit: &RoundUpSubmit,
	) -> TransactionRequest {
		TransactionRequest::default()
			.to(target_socket.address())
			.data(target_socket.round_control_relay(roundup_submit.clone()).calldata().unwrap())
	}

	/// Check roundup submitted before. If not, call `round_control_relay`.
	async fn broadcast_roundup(&self, roundup_submit: &RoundUpSubmit) {
		if self.external_clients.is_empty() {
			return
		}

		let mut stream = tokio_stream::iter(self.external_clients.iter());
		while let Some(target_client) = stream.next().await {
			// Check roundup submitted to target chain before.
			let latest_round = target_client
				.contract_call(
					target_client.contracts.authority.latest_round(),
					"authority.latest_round",
				)
				.await;
			if roundup_submit.round > latest_round {
				let transaction_request =
					self.build_transaction_request(&target_client.contracts.socket, roundup_submit);

				if let Some(event_sender) = self.event_senders.get(&target_client.get_chain_id()) {
					event_sender
						.send(EventMessage::new(
							TxRequest::Legacy(transaction_request),
							EventMetadata::VSPPhase2(VSPPhase2Metadata::new(
								roundup_submit.round,
								target_client.get_chain_id(),
							)),
							true,
							true,
							GasCoefficient::Low,
						))
						.unwrap()
				}
			}
		}
	}

	/// Check if external clients are in the latest round.
	async fn wait_if_latest_round(&self) {
		let barrier_clone = self.roundup_barrier.clone();
		let external_clients = &self.external_clients;

		for target_client in external_clients {
			let barrier_clone_inner = barrier_clone.clone();
			let current_round = self
				.client
				.contract_call(
					self.client.contracts.authority.latest_round(),
					"authority.latest_round",
				)
				.await;
			let target_chain_round = target_client
				.contract_call(
					target_client.contracts.authority.latest_round(),
					"authority.latest_round",
				)
				.await;
			let bootstrap_guard = self.bootstrapping_count.clone();

			tokio::spawn(async move {
				if current_round == target_chain_round {
					*bootstrap_guard.lock().await += 1;
				}
				barrier_clone_inner.wait().await;
			});
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for RoundupRelayHandler<T> {
	async fn bootstrap(&self) {
		log::info!(
			target: &self.client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping RoundUp events.",
			sub_display_format(SUB_LOG_TARGET),
		);

		let mut bootstrap_guard = self.bootstrap_states.write().await;
		// Checking if the current round is the latest round
		self.wait_if_latest_round().await;

		// Wait to lock after checking if it is latest round
		self.roundup_barrier.clone().wait().await;

		// if all of chain is the latest round already
		if *self.bootstrapping_count.lock().await == self.external_clients.len() as u8 {
			// set all of state to BootstrapSocket
			for state in bootstrap_guard.iter_mut() {
				*state = BootstrapState::BootstrapBridgeRelay;
			}
		}

		if bootstrap_guard.iter().all(|s| *s == BootstrapState::BootstrapRoundUpPhase2) {
			drop(bootstrap_guard);
			let logs = self.get_bootstrap_events().await;

			let mut stream = tokio_stream::iter(logs);
			while let Some(log) = stream.next().await {
				self.process_confirmed_log(&log, true).await;
			}
		}

		// Poll socket barrier to call wait()
		let socket_barrier_clone = self.socket_barrier.clone();

		tokio::spawn(async move {
			socket_barrier_clone.clone().wait().await;
		});
	}

	async fn get_bootstrap_events(&self) -> Vec<Log> {
		let mut logs = vec![];

		if let Some(bootstrap_config) = &self.bootstrap_config {
			let round_info: RoundMetaData = self
				.client
				.contract_call(self.client.contracts.authority.round_info(), "authority.round_info")
				.await;
			let bootstrap_offset_height = self
				.client
				.get_bootstrap_offset_height_based_on_block_time(
					bootstrap_config.round_offset.unwrap_or(BOOTSTRAP_DEFAULT_ROUND_OFFSET),
					round_info,
				)
				.await;

			let latest_block_number = self.client.get_latest_block_number().await;
			let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
			let to_block = latest_block_number;

			// Split from_block into smaller chunks
			while from_block <= to_block {
				let chunk_to_block =
					std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

				let filter = Filter::new()
					.address(self.client.contracts.socket.address())
					.topic0(self.roundup_signature)
					.from_block(from_block)
					.to_block(chunk_to_block);

				let chunk_logs = self.client.get_logs(&filter).await;
				logs.extend(chunk_logs);

				from_block = chunk_to_block + 1;
			}
		}

		logs
	}

	/// Verifies whether the bootstrap state has been synced to the given state.
	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_states.read().await.iter().all(|s| *s == state)
	}
}
