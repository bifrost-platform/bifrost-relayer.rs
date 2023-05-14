use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler, VSPPhase2Metadata,
};
use async_trait::async_trait;
use cccp_primitives::{
	authority::RoundMetaData,
	cli::BootstrapConfig,
	eth::{
		BootstrapState, RecoveredSignature, RoundUpEventStatus, BOOTSTRAP_BLOCK_CHUNK_SIZE,
		BOOTSTRAP_BLOCK_OFFSET, NATIVE_BLOCK_TIME,
	},
	socket::{RoundUpSubmit, SerializedRoundUp, Signatures, SocketContract, SocketContractEvents},
	sub_display_format, INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_CONTRACT_ABI,
};
use ethers::{
	abi::{encode, Detokenize, Token, Tokenize},
	contract::EthLogDecode,
	prelude::{TransactionReceipt, H256},
	providers::{JsonRpcClient, Provider},
	types::{Address, Bytes, Filter, Log, Signature, TransactionRequest, H160, U256, U64},
};
use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};
use tokio::{
	sync::{broadcast::Receiver, Barrier, Mutex, RwLock},
	time::sleep,
};
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "roundup-handler";

/// The essential task that handles `roundup relay` related events.
pub struct RoundupRelayHandler<T> {
	/// The event senders that sends messages to each event channel.
	event_senders: BTreeMap<u32, Arc<EventSender>>,
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
	pub bootstrap_config: BootstrapConfig,
}

#[async_trait]
impl<T: JsonRpcClient> Handler for RoundupRelayHandler<T> {
	async fn run(&mut self) {
		loop {
			if self
				.bootstrap_states
				.read()
				.await
				.iter()
				.all(|s| *s == BootstrapState::BootstrapRoundUp)
			{
				self.bootstrap().await;

				sleep(Duration::from_millis(self.client.config.call_interval)).await;
			} else if self
				.bootstrap_states
				.read()
				.await
				.iter()
				.all(|s| *s == BootstrapState::NormalStart)
			{
				let block_msg = self.block_receiver.recv().await.unwrap();

				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 📦 Imported #{:?} ({}) with target transactions({:?})",
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
				Ok(serialized_log) => {
					let status = RoundUpEventStatus::from_u8(serialized_log.status);
					log::info!(
						target: &self.client.get_chain_name(),
						"-[{}] 👤 RoundUp event detected. ({:?}-{:?})",
						sub_display_format(SUB_LOG_TARGET),
						serialized_log.status,
						receipt.transaction_hash,
					);
					match status {
						RoundUpEventStatus::NextAuthorityCommitted => {
							let roundup_submit = self
								.build_roundup_submit(
									serialized_log.roundup.round,
									serialized_log.roundup.new_relayers,
								)
								.await;
							self.broadcast_roundup(roundup_submit).await;
						},
						RoundUpEventStatus::NextAuthorityRelayed => continue,
					}
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
				ethers::utils::to_checksum(&self.client.socket.address(), None)
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
		event_senders_vec: Vec<Arc<EventSender>>,
		block_receiver: Receiver<BlockMessage>,
		clients: Vec<Arc<EthClient<T>>>,
		socket_barrier: Arc<Barrier>,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
		bootstrap_config: BootstrapConfig,
		number_of_relay_targets: usize,
	) -> Self {
		let mut event_senders = BTreeMap::new();
		event_senders_vec.iter().for_each(|event_sender| {
			event_senders.insert(event_sender.id, event_sender.clone());
		});

		let client = clients
			.iter()
			.find(|client| client.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone();

		let external_clients = clients.into_iter().filter(|client| !client.is_native).collect();

		let roundup_signature =
			client.socket.abi().event("RoundUp").expect(INVALID_CONTRACT_ABI).signature();

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
	async fn get_sorted_signatures(&self, round: U256, new_relayers: Vec<Address>) -> Signatures {
		let encoded_msg = encode(&[
			Token::Uint(round),
			Token::Array(new_relayers.iter().map(|address| Token::Address(*address)).collect()),
		]);

		let unordered_sigs = self.client.socket.get_round_signatures(round).call().await.unwrap();
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
		target_contract: SocketContract<Provider<T>>,
		roundup_submit: RoundUpSubmit,
	) -> TransactionRequest {
		TransactionRequest::default()
			.to(target_contract.address())
			.data(target_contract.round_control_relay(roundup_submit).calldata().unwrap())
	}

	/// Check roundup submitted before. If not, call `round_control_relay`.
	async fn broadcast_roundup(&self, roundup_submit: RoundUpSubmit) {
		let mut stream = tokio_stream::iter(self.external_clients.iter());
		while let Some(target_client) = stream.next().await {
			// Check roundup submitted to target chain before.
			if roundup_submit.round > target_client.authority.latest_round().call().await.unwrap() {
				let transaction_request = self.build_transaction_request(
					target_client.socket.clone(),
					roundup_submit.clone(),
				);

				let event_sender =
					self.event_senders.get(&target_client.get_chain_id()).expect(INVALID_CHAIN_ID);

				event_sender
					.send(EventMessage::new(
						transaction_request,
						EventMetadata::VSPPhase2(VSPPhase2Metadata::new(
							roundup_submit.round,
							target_client.get_chain_id(),
						)),
						true,
					))
					.unwrap()
			}
		}
	}

	async fn wait_if_latest_round(&self) {
		let barrier_clone = self.roundup_barrier.clone();
		let external_clients = self.external_clients.clone();

		for target_client in external_clients {
			let barrier_clone_inner = barrier_clone.clone();
			let current_round = self.client.authority.latest_round().call().await.unwrap();
			let target_chain_round = target_client.authority.latest_round().call().await.unwrap();
			let bootstrap_guard = self.bootstrapping_count.clone();

			tokio::spawn(async move {
				if current_round == target_chain_round {
					*bootstrap_guard.lock().await += 1;
				}
				barrier_clone_inner.wait().await;
			});
		}
	}

	async fn bootstrap(&self) {
		let mut bootstrap_guard = self.bootstrap_states.write().await;
		// Checking if the current round is the latest round
		self.wait_if_latest_round().await;

		// Wait to lock after checking if it is latest round
		self.roundup_barrier.clone().wait().await;

		// if all of chain is the latest round already
		// TODO: Review: Is this refactor correct?
		if *self.bootstrapping_count.lock().await == self.external_clients.len() as u8 {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] ⚙️  [Bootstrap mode] Bootstrapping RoundUp events.",
				sub_display_format(SUB_LOG_TARGET),
			);

			// set all of state to BootstrapSocket
			for state in bootstrap_guard.iter_mut() {
				*state = BootstrapState::BootstrapSocket;
			}
		}

		if bootstrap_guard.iter().all(|s| *s == BootstrapState::BootstrapRoundUp) {
			drop(bootstrap_guard);
			let logs = self.get_roundup_logs().await;

			let mut stream = tokio_stream::iter(logs);
			while let Some(log) = stream.next().await {
				if let Some(receipt) =
					self.client.get_transaction_receipt(log.transaction_hash.unwrap()).await
				{
					self.process_confirmed_transaction(receipt).await;
				}
			}
		}

		// Poll socket barrier to call wait()
		let socket_barrier_clone = self.socket_barrier.clone();

		tokio::spawn(async move {
			socket_barrier_clone.clone().wait().await;
		});
	}

	async fn get_roundup_logs(&self) -> Vec<Log> {
		let bootstrap_offset_height = self
			.get_bootstrap_offset_height_based_on_block_time(self.bootstrap_config.round_offset)
			.await;

		let latest_block_number = self.client.get_latest_block_number().await;
		let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
		let to_block = latest_block_number;

		let mut logs = vec![];

		// Split from_block into smaller chunks
		while from_block <= to_block {
			let chunk_to_block =
				std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

			let filter = Filter::new()
				.address(self.client.socket.address())
				.topic0(self.roundup_signature)
				.from_block(from_block)
				.to_block(chunk_to_block);

			let chunk_logs = self.client.get_logs(filter).await;
			logs.extend(chunk_logs);

			from_block = chunk_to_block + 1;
		}

		logs
	}

	/// Get factor between the block time of native-chain and block time of this chain
	/// Approximately bfc-testnet: 3s, matic-mumbai: 2s, bsc-testnet: 3s, eth-goerli: 12s
	pub async fn get_bootstrap_offset_height_based_on_block_time(&self, round_offset: u32) -> U64 {
		let round_info: RoundMetaData = self.client.authority.round_info().call().await.unwrap();

		let block_number = self.client.get_latest_block_number().await;

		let current_block = self.client.get_block((block_number).into()).await.unwrap();
		let prev_block = self
			.client
			.get_block((block_number - BOOTSTRAP_BLOCK_OFFSET).into())
			.await
			.unwrap();

		let diff = current_block
			.timestamp
			.checked_sub(prev_block.timestamp)
			.unwrap()
			.checked_div(BOOTSTRAP_BLOCK_OFFSET.into())
			.unwrap();

		round_offset
			.checked_mul(round_info.round_length.as_u32())
			.unwrap()
			.checked_mul(NATIVE_BLOCK_TIME)
			.unwrap()
			.checked_div(diff.as_u32())
			.unwrap()
			.into()
	}
}
