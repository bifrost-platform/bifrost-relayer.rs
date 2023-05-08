use crate::eth::{
	BlockMessage, EthClient, EventMessage, EventMetadata, EventSender, Handler, VSPPhase2Metadata,
};
use async_trait::async_trait;
use cccp_primitives::{
	authority_bifrost::{AuthorityBifrost, RoundMetaData},
	authority_external::AuthorityExternal,
	cli::{BootstrapConfig, RoundupHandlerUtilityConfig},
	eth::{BootstrapState, RecoveredSignature, RoundUpEventStatus},
	socket_bifrost::{SerializedRoundUp, SocketBifrost, SocketBifrostEvents},
	socket_external::{RoundUpSubmit, Signatures, SocketExternal},
	sub_display_format, RoundupHandlerUtilType,
};
use ethers::{
	abi::{encode, Detokenize, Token, Tokenize},
	contract::EthLogDecode,
	prelude::{TransactionReceipt, H256},
	providers::{JsonRpcClient, Middleware, Provider},
	types::{Address, Bytes, Filter, Log, Signature, TransactionRequest, H160, U256, U64},
};
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::{
	sync::{broadcast::Receiver, Barrier, Mutex, RwLock},
	time::sleep,
};
use tokio_stream::StreamExt;

const SUB_LOG_TARGET: &str = "roundup-handler";
const NATIVE_BLOCK_TIME: u32 = 3u32;

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
	pub roundup_utils: Arc<Vec<RoundupUtility<T>>>,
	/// Signature of RoundUp Event.
	pub roundup_signature: H256,
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
	/// Authority contracts on native chain
	pub authority_bifrost: AuthorityBifrost<Provider<T>>,
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
		socket_barrier: Arc<Barrier>,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
		bootstrap_config: BootstrapConfig,
		authority_address: String,
	) -> Self {
		let roundup_signature = socket_bifrost.abi().event("RoundUp").unwrap().signature();
		let roundup_utils = Arc::new(
			event_senders
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
												.find(|client| {
													client.get_chain_id() == config.chain_id
												})
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
												.find(|client| {
													client.get_chain_id() == config.chain_id
												})
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

					let socket_external =
						socket_external.expect("socket_external must be initialized");
					let authority_external =
						authority_external.expect("authority_external must be initialized");

					RoundupUtility {
						event_sender: sender.clone(),
						socket_external,
						authority_external,
						id: sender.id,
					}
				})
				.collect(),
		);

		let authority_bifrost = AuthorityBifrost::new(
			H160::from_str(&authority_address).unwrap(),
			client.get_provider(),
		);

		let roundup_barrier = Arc::new(Barrier::new(external_clients.len() + 1));
		let bootstrapping_count = Arc::new(Mutex::new(u8::default()));

		Self {
			block_receiver,
			client,
			socket_bifrost,
			roundup_utils,
			roundup_signature,
			socket_barrier,
			roundup_barrier,
			bootstrap_states,
			bootstrapping_count,
			bootstrap_config,
			authority_bifrost,
		}
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
		let roundup_utils = self.roundup_utils.clone();
		let mut stream = tokio_stream::iter(roundup_utils.iter());
		while let Some(target_chain) = stream.next().await {
			// Check roundup submitted to target chain before.
			if roundup_submit.round >
				target_chain.authority_external.latest_round().call().await.unwrap()
			{
				let transaction_request = self.build_transaction_request(
					target_chain.socket_external.clone(),
					roundup_submit.clone(),
				);

				target_chain
					.event_sender
					.sender
					.send(EventMessage::new(
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

	async fn wait_if_latest_round(&self) {
		let barrier_clone = self.roundup_barrier.clone();
		let roundup_utils = &self.roundup_utils.clone();

		for target_chain in roundup_utils.iter() {
			let barrier_clone_inner = barrier_clone.clone();
			let current_round = self.authority_bifrost.latest_round().call().await.unwrap();
			let target_chain_round =
				target_chain.authority_external.latest_round().call().await.unwrap();
			let chain_name = target_chain.id;
			let bootstrap_guard = self.bootstrapping_count.clone();

			tokio::spawn(async move {
				if current_round == target_chain_round {
					log::info!(
						target: "bootstrapping",
						"-[{}] Chain {} is already in the latest round",
						sub_display_format(SUB_LOG_TARGET),
						chain_name,
					);

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
		if *self.bootstrapping_count.lock().await == self.roundup_utils.len() as u8 {
			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] Roundup -> Socket Bootstrapping",
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
				let receipt = self
					.client
					.provider
					.get_transaction_receipt(log.transaction_hash.unwrap())
					.await
					.unwrap()
					.unwrap();

				self.process_confirmed_transaction(receipt).await;
			}
		}

		// Poll socket barrier to call wait()
		let socket_barrier_clone = self.socket_barrier.clone();

		tokio::spawn(async move {
			socket_barrier_clone.clone().wait().await;
		});
	}

	async fn get_roundup_logs(&self) -> Vec<Log> {
		let roundup_signature = self.socket_bifrost.abi().event("RoundUp").unwrap().signature();

		let bootstrap_offset_height = self
			.get_bootstrap_offset_height_based_on_block_time(self.bootstrap_config.round_offset)
			.await;

		let latest_block_number = self.client.get_latest_block_number().await.unwrap();
		let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
		let to_block = latest_block_number;

		let mut logs = vec![];

		// Split from_block into smaller chunks
		let block_chunk_size = 2000;
		while from_block <= to_block {
			let chunk_to_block = std::cmp::min(from_block + block_chunk_size - 1, to_block);

			let filter = Filter::new()
				.address(self.socket_bifrost.address())
				.event("RoundUp(uint8,(uint256,address[],(bytes32[],bytes32[],bytes)))")
				.topic0(roundup_signature)
				.from_block(from_block)
				.to_block(chunk_to_block);

			let chunk_logs = self.client.provider.get_logs(&filter).await.unwrap();
			logs.extend(chunk_logs);

			from_block = chunk_to_block + 1;
		}

		logs
	}

	/// Get factor between the block time of native-chain and block time of this chain
	/// Approximately bfc-testnet: 3s, matic-mumbai: 2s, bsc-testnet: 3s, eth-goerli: 12s
	pub async fn get_bootstrap_offset_height_based_on_block_time(&self, round_offset: u32) -> U64 {
		let block_offset = 100u32;
		let round_info: RoundMetaData = self.authority_bifrost.round_info().call().await.unwrap();

		let block_number = self.client.provider.get_block_number().await.unwrap();

		let current_block = self.client.get_block((block_number).into()).await.unwrap().unwrap();
		let prev_block = self
			.client
			.get_block((block_number - block_offset).into())
			.await
			.unwrap()
			.unwrap();

		let diff = current_block
			.timestamp
			.checked_sub(prev_block.timestamp)
			.unwrap()
			.checked_div(block_offset.into())
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
