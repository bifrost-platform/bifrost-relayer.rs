use std::{str::FromStr, sync::Arc, time::Duration};

use cron::Schedule;
use ethers::{
	abi::{encode, Detokenize, Token, Tokenize},
	contract::EthLogDecode,
	providers::JsonRpcClient,
	types::{Address, Filter, Log, TransactionRequest, U256},
};
use tokio::{sync::RwLock, time::sleep};

use br_client::eth::{
	BootstrapHandler, EthClient, EventMessage, EventMetadata, EventSender, TxRequest,
	VSPPhase1Metadata,
};
use br_primitives::{
	authority::RoundMetaData,
	cli::{BootstrapConfig, BOOTSTRAP_DEFAULT_ROUND_OFFSET},
	errors::INVALID_PERIODIC_SCHEDULE,
	eth::{BootstrapState, GasCoefficient, RoundUpEventStatus, BOOTSTRAP_BLOCK_CHUNK_SIZE},
	socket::{RoundUpSubmit, SerializedRoundUp, Signatures, SocketContractEvents},
	sub_display_format, PeriodicWorker, INVALID_BIFROST_NATIVENESS, INVALID_CONTRACT_ABI,
};

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
	pub bootstrap_config: Option<BootstrapConfig>,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for RoundupEmitter<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		self.current_round = self.get_latest_round().await;

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapRoundUpPhase1).await {
				self.bootstrap().await;
				break
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				break
			}

			sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
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

				if !self.is_selected_relayer(latest_round).await {
					continue
				}

				let new_relayers = self.fetch_validator_list(latest_round).await;
				self.request_send_transaction(
					self.build_transaction(latest_round, new_relayers.clone()),
					VSPPhase1Metadata::new(latest_round, new_relayers),
				);

				self.current_round = latest_round;
			}
		}
	}
}

impl<T: JsonRpcClient> RoundupEmitter<T> {
	/// Instantiates a new `RoundupEmitter` instance.
	pub fn new(
		event_senders: Vec<Arc<EventSender>>,
		clients: Vec<Arc<EthClient<T>>>,
		schedule: String,
		bootstrap_states: Arc<RwLock<Vec<BootstrapState>>>,
		bootstrap_config: Option<BootstrapConfig>,
	) -> Self {
		let client = clients
			.iter()
			.find(|client| client.metadata.is_native)
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

	/// Decode & Serialize log to `SerializedRoundUp` struct.
	fn decode_log(&self, log: Log) -> Result<SerializedRoundUp, ethers::abi::Error> {
		match SocketContractEvents::decode_log(&log.into()) {
			Ok(roundup) => Ok(SerializedRoundUp::from_tokens(roundup.into_tokens()).unwrap()),
			Err(error) => Err(error),
		}
	}

	/// Check relayer has selected in previous round
	async fn is_selected_relayer(&self, round: U256) -> bool {
		let relayer_manager = self.client.contracts.relayer_manager.as_ref().unwrap();
		self.client
			.contract_call(
				relayer_manager.is_previous_selected_relayer(
					round - 1,
					self.client.address(),
					true,
				),
				"relayer_manager.is_previous_selected_relayer",
			)
			.await
	}

	/// Fetch new validator list
	async fn fetch_validator_list(&self, round: U256) -> Vec<Address> {
		let relayer_manager = self.client.contracts.relayer_manager.as_ref().unwrap();
		let mut addresses = self
			.client
			.contract_call(
				relayer_manager.previous_selected_relayers(round, true),
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

		TransactionRequest::default().to(self.client.contracts.socket.address()).data(
			self.client
				.contracts
				.socket
				.round_control_poll(round_up_submit)
				.calldata()
				.unwrap(),
		)
	}

	/// Request send transaction to the target event channel.
	fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: VSPPhase1Metadata,
	) {
		match self.event_sender.send(EventMessage::new(
			TxRequest::Legacy(tx_request),
			EventMetadata::VSPPhase1(metadata.clone()),
			false,
			false,
			GasCoefficient::Mid,
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

	/// Get the latest round index.
	async fn get_latest_round(&self) -> U256 {
		self.client
			.contract_call(self.client.contracts.authority.latest_round(), "authority.latest_round")
			.await
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> BootstrapHandler for RoundupEmitter<T> {
	async fn bootstrap(&self) {
		let get_next_poll_round = || async move {
			let logs = self.get_bootstrap_events().await;
			let round_up_events: Vec<SerializedRoundUp> =
				logs.iter().map(|log| self.decode_log(log.clone()).unwrap()).collect();

			let max_round = round_up_events
				.iter()
				.map(|round_up_event| round_up_event.roundup.round)
				.max()
				.unwrap();
			let max_events: Vec<&SerializedRoundUp> = round_up_events
				.iter()
				.filter(|round_up_event| round_up_event.roundup.round == max_round)
				.collect();

			let status = RoundUpEventStatus::from_u8(
				max_events.iter().map(|round_up| round_up.status).max().unwrap(),
			);

			match status {
				RoundUpEventStatus::NextAuthorityRelayed => max_round,
				RoundUpEventStatus::NextAuthorityCommitted => max_round + 1,
			}
		};

		let mut next_poll_round = get_next_poll_round().await;

		loop {
			if next_poll_round == self.current_round + 1 {
				// If RoundUp reached to latest round, escape loop
				break
			} else if next_poll_round <= self.current_round {
				// If RoundUp not reached to latest round, process round_control_poll
				if self.is_selected_relayer(next_poll_round).await {
					let new_relayers = self.fetch_validator_list(next_poll_round).await;
					self.request_send_transaction(
						self.build_transaction(next_poll_round, new_relayers.clone()),
						VSPPhase1Metadata::new(next_poll_round, new_relayers),
					);
				}

				// Wait for RoundUp event's status changes via RoundUpSubmit right before
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 👤 VSP phase1 is still in progress. The majority must reach quorum to move on to phase2.",
					sub_display_format(SUB_LOG_TARGET),
				);
				loop {
					let new_next_poll_round = get_next_poll_round().await;

					if next_poll_round < new_next_poll_round {
						next_poll_round = new_next_poll_round;
						break
					}

					sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
				}
			}
		}

		for state in self.bootstrap_states.write().await.iter_mut() {
			if *state == BootstrapState::BootstrapRoundUpPhase1 {
				*state = BootstrapState::BootstrapRoundUpPhase2;
			}
		}
	}

	async fn get_bootstrap_events(&self) -> Vec<Log> {
		let mut round_up_events = vec![];

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
					.topic0(
						self.client
							.contracts
							.socket
							.abi()
							.event("RoundUp")
							.expect(INVALID_CONTRACT_ABI)
							.signature(),
					)
					.from_block(from_block)
					.to_block(chunk_to_block);

				let chunk_logs = self.client.get_logs(&filter).await;
				round_up_events.extend(chunk_logs);

				from_block = chunk_to_block + 1;
			}

			if round_up_events.is_empty() {
				panic!(
					"[{}]-[{}]-[{}] ❗️ Failed to find the latest RoundUp event. Please use a higher bootstrap offset. Current offset: {:?}",
					self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					bootstrap_config.round_offset.unwrap_or(BOOTSTRAP_DEFAULT_ROUND_OFFSET)
				);
			}
		}

		round_up_events
	}

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_states.read().await.iter().all(|s| *s == state)
	}
}
