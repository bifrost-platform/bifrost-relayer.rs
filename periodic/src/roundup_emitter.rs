use cccp_client::eth::{EthClient, EventMessage, EventMetadata, EventSender, VSPPhase1Metadata};
use cccp_primitives::{
	authority::RoundMetaData,
	cli::BootstrapConfig,
	errors::INVALID_PERIODIC_SCHEDULE,
	eth::{
		BootstrapState, RoundUpEventStatus, BOOTSTRAP_BLOCK_CHUNK_SIZE, BOOTSTRAP_BLOCK_OFFSET,
		NATIVE_BLOCK_TIME,
	},
	socket::{RoundUpSubmit, SerializedRoundUp, Signatures, SocketContractEvents},
	sub_display_format, PeriodicWorker, INVALID_BIFROST_NATIVENESS, INVALID_CONTRACT_ABI,
};
use cron::Schedule;
use ethers::{
	abi::{encode, Detokenize, Token, Tokenize},
	contract::EthLogDecode,
	providers::JsonRpcClient,
	types::{Address, Filter, Log, TransactionRequest, U256, U64},
};
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::{sync::RwLock, time::sleep};

const SUB_LOG_TARGET: &str = "roundup-emitter";

pub struct RoundupEmitter<T> {
	/// Current round number
	pub current_round: U256,
	/// The ethereum client for the Bifrost network.
	pub client: Arc<EthClient<T>>,
	/// The event sender that sends messages to the event channel.
	pub event_sender: Arc<EventSender>,
	/// The time schedule that represents when to check round info.
	pub schedule: Schedule,
	/// State of bootstrapping
	pub bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
	/// Bootstrap config
	pub bootstrap_config: BootstrapConfig,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for RoundupEmitter<T> {
	async fn run(&mut self) {
		self.current_round = self.get_latest_round().await;

		loop {
			if self
				.bootstrap_states
				.read()
				.await
				.iter()
				.all(|s| *s == BootstrapState::BootstrapRoundUp2)
			{
				self.bootstrap().await;
				break
			} else if self
				.bootstrap_states
				.read()
				.await
				.iter()
				.all(|s| *s == BootstrapState::NormalStart)
			{
				break
			}

			sleep(Duration::from_millis(self.client.call_interval)).await;
		}

		loop {
			self.wait_until_next_time().await;

			let latest_round = self.get_latest_round().await;

			if self.current_round < latest_round {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 👤 RoundUp detected. Round({}) -> Round({})",
					sub_display_format(SUB_LOG_TARGET),
					self.current_round,
					latest_round,
				);

				self.current_round = latest_round;

				if !self.is_selected_relayer().await {
					continue
				}

				let new_relayers = self.fetch_validator_list().await;
				self.request_send_transaction(
					self.build_transaction(latest_round, new_relayers.clone()),
					VSPPhase1Metadata::new(latest_round, new_relayers),
				);
			}
		}
	}

	async fn wait_until_next_time(&self) {
		let sleep_duration =
			self.schedule.upcoming(chrono::Utc).next().unwrap() - chrono::Utc::now();

		sleep(sleep_duration.to_std().unwrap()).await;
	}
}

impl<T: JsonRpcClient> RoundupEmitter<T> {
	/// Instantiates a new `RoundupEmitter` instance.
	pub fn new(
		event_senders: Vec<Arc<EventSender>>,
		clients: Vec<Arc<EthClient<T>>>,
		schedule: String,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
		bootstrap_config: BootstrapConfig,
	) -> Self {
		let client = clients
			.iter()
			.find(|client| client.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.clone();

		Self {
			current_round: U256::default(),
			client,
			event_sender: event_senders
				.iter()
				.find(|event_sender| event_sender.is_native)
				.expect(INVALID_BIFROST_NATIVENESS)
				.clone(),
			schedule: Schedule::from_str(&schedule).expect(INVALID_PERIODIC_SCHEDULE),
			bootstrap_states,
			bootstrap_config,
		}
	}

	async fn bootstrap(&self) {
		if self.is_selected_relayer().await {
			let next_poll_round =
				self.get_next_poll_round(self.bootstrap_config.round_offset).await;

			loop {
				if next_poll_round == self.current_round + 1 {
					break
				} else if next_poll_round <= self.current_round {
					let new_relayers = self.fetch_validator_list().await;
					self.request_send_transaction(
						self.build_transaction(next_poll_round, new_relayers.clone()),
						VSPPhase1Metadata::new(next_poll_round, new_relayers),
					);

					loop {
						if next_poll_round < self.get_next_poll_round(1).await {
							break
						}
						sleep(Duration::from_millis(self.client.call_interval)).await;
					}
				}
			}
		}

		for state in self.bootstrap_states.write().await.iter_mut() {
			match *state {
				BootstrapState::BootstrapRoundUp1 => {
					*state = BootstrapState::BootstrapRoundUp2;
					return
				},
				_ => return,
			}
		}
	}

	async fn get_next_poll_round(&self, offset: u32) -> U256 {
		let bootstrap_offset_height =
			self.get_bootstrap_offset_height_based_on_block_time(offset).await;

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
				.topic0(
					self.client
						.socket
						.abi()
						.event("RoundUp")
						.expect(INVALID_CONTRACT_ABI)
						.signature(),
				)
				.from_block(from_block)
				.to_block(chunk_to_block);

			let chunk_logs: Vec<SerializedRoundUp> = self
				.client
				.get_logs(filter)
				.await
				.iter()
				.map(|log| self.decode_log(log.clone()).unwrap())
				.collect();
			logs.extend(chunk_logs);

			from_block = chunk_to_block + 1;
		}

		if logs.is_empty() {
			panic!("Panic on BootstrapRoundUp1. Use higher bootstrap offset");
		}

		let max_round = logs.iter().map(|round_up| round_up.roundup.round).max().unwrap();
		let max_logs: Vec<&SerializedRoundUp> =
			logs.iter().filter(|log| log.roundup.round == max_round).collect();

		let status = RoundUpEventStatus::from_u8(
			max_logs.iter().map(|round_up| round_up.status).max().unwrap(),
		);

		match status {
			RoundUpEventStatus::NextAuthorityRelayed => max_round,
			RoundUpEventStatus::NextAuthorityCommitted => max_round + 1,
		}
	}

	/// Get factor between the block time of native-chain and block time of this chain
	/// Approximately BIFROST: 3s, Polygon: 2s, BSC: 3s, Ethereum: 12s
	pub async fn get_bootstrap_offset_height_based_on_block_time(&self, round_offset: u32) -> U64 {
		let round_info: RoundMetaData = self
			.client
			.contract_call(self.client.authority.round_info(), "authority.round_info")
			.await;

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

	/// Decode & Serialize log to `SerializedRoundUp` struct.
	fn decode_log(&self, log: Log) -> Result<SerializedRoundUp, ethers::abi::Error> {
		match SocketContractEvents::decode_log(&log.into()) {
			Ok(roundup) => Ok(SerializedRoundUp::from_tokens(roundup.into_tokens()).unwrap()),
			Err(error) => Err(error),
		}
	}

	/// Check relayer has selected in previous round
	async fn is_selected_relayer(&self) -> bool {
		let relayer_manager = self.client.relayer_manager.as_ref().unwrap();
		self.client
			.contract_call(
				relayer_manager.is_previous_selected_relayer(
					self.current_round - 1,
					self.client.address(),
					true,
				),
				"relayer_manager.is_previous_selected_relayer",
			)
			.await
	}

	/// Fetch new validator list
	async fn fetch_validator_list(&self) -> Vec<Address> {
		let relayer_manager = self.client.relayer_manager.as_ref().unwrap();
		let mut addresses = self
			.client
			.contract_call(
				relayer_manager.selected_relayers(true),
				"relayer_manager.selected_relayers",
			)
			.await;
		addresses.sort();
		addresses
	}

	/// Build `VSP phase 1` transaction.
	fn build_transaction(&self, round: U256, new_relayers: Vec<Address>) -> TransactionRequest {
		let encoded_msg = encode(&[
			Token::Uint(round),
			Token::Array(new_relayers.iter().map(|address| Token::Address(*address)).collect()),
		]);
		let sigs = Signatures::from(self.client.wallet.sign_message(&encoded_msg));
		let round_up_submit = RoundUpSubmit { round, new_relayers, sigs };

		TransactionRequest::default()
			.to(self.client.socket.address())
			.data(self.client.socket.round_control_poll(round_up_submit).calldata().unwrap())
	}

	/// Request send transaction to the target event channel.
	fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: VSPPhase1Metadata,
	) {
		match self.event_sender.send(EventMessage::new(
			tx_request,
			EventMetadata::VSPPhase1(metadata.clone()),
			false,
			false,
		)) {
			Ok(()) => log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 👤 Request VSP phase1 transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => log::error!(
				target: &self.client.get_chain_name(),
				"-[{}] ❗️ Failed to request VSP phase1 transaction: {}, Error: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata,
				error.to_string()
			),
		}
	}

	async fn get_latest_round(&self) -> U256 {
		self.client
			.contract_call(self.client.authority.latest_round(), "authority.latest_round")
			.await
	}
}
