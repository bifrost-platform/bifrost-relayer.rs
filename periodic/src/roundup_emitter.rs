use alloy::{
	dyn_abi::DynSolValue,
	network::AnyNetwork,
	primitives::{Address, U256},
	providers::{fillers::TxFiller, Provider, WalletProvider},
	rpc::types::{Filter, Log, TransactionInput, TransactionRequest},
	sol_types::SolEvent as _,
	transports::Transport,
};
use cron::Schedule;
use sc_service::SpawnTaskHandle;
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::time::sleep;

use br_client::eth::{send_transaction, traits::BootstrapHandler, EthClient};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET, config::BOOTSTRAP_BLOCK_CHUNK_SIZE,
		errors::INVALID_PERIODIC_SCHEDULE, schedule::ROUNDUP_EMITTER_SCHEDULE,
	},
	contracts::socket::{
		SocketContract::RoundUp,
		Socket_Struct::{Round_Up_Submit, Signatures},
	},
	eth::{BootstrapState, RoundUpEventStatus},
	tx::{TxRequestMetadata, VSPPhase1Metadata},
	utils::sub_display_format,
};
use eyre::Result;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "roundup-emitter";

pub struct RoundupEmitter<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Current round number
	current_round: U256,
	/// The ethereum client for the Bifrost network.
	pub client: Arc<EthClient<F, P, T>>,
	/// The time schedule that represents when to check round info.
	schedule: Schedule,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The handle to spawn tasks.
	handle: SpawnTaskHandle,
}

#[async_trait::async_trait]
impl<F, P, T> PeriodicWorker for RoundupEmitter<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		self.current_round = self.get_latest_round().await?;

		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapRoundUpPhase1).await {
				self.bootstrap().await?;
				break;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				break;
			}

			sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
		}

		loop {
			self.wait_until_next_time().await;

			let latest_round = self.get_latest_round().await?;

			if self.current_round < latest_round {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 👤 RoundUp detected. Round({}) -> Round({})",
					sub_display_format(SUB_LOG_TARGET),
					self.current_round,
					latest_round,
				);

				if !self.is_selected_relayer(latest_round).await? {
					continue;
				}

				let new_relayers = self.fetch_validator_list(latest_round).await?;
				self.request_send_transaction(
					self.build_transaction(latest_round, new_relayers.clone()).await?,
					VSPPhase1Metadata::new(latest_round, new_relayers),
				);

				self.current_round = latest_round;
			}
		}
	}
}

impl<F, P, T> RoundupEmitter<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	/// Instantiates a new `RoundupEmitter` instance.
	pub fn new(
		client: Arc<EthClient<F, P, T>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
		handle: SpawnTaskHandle,
	) -> Self {
		Self {
			current_round: U256::default(),
			client,
			schedule: Schedule::from_str(ROUNDUP_EMITTER_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
			bootstrap_shared_data,
			handle,
		}
	}

	/// Check relayer has selected in previous round
	async fn is_selected_relayer(&self, round: U256) -> Result<bool> {
		let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
		Ok(relayer_manager
			.is_previous_selected_relayer(round - U256::from(1), self.client.address(), true)
			.call()
			.await?
			._0)
	}

	/// Fetch new validator list
	async fn fetch_validator_list(&self, round: U256) -> Result<Vec<Address>> {
		let relayer_manager = self.client.protocol_contracts.relayer_manager.as_ref().unwrap();
		let mut addresses =
			relayer_manager.previous_selected_relayers(round, true).call().await?._0;
		addresses.sort();
		Ok(addresses)
	}

	/// Build `VSP phase 1` transaction.
	async fn build_transaction(
		&self,
		round: U256,
		new_relayers: Vec<Address>,
	) -> Result<TransactionRequest> {
		let encoded_msg = DynSolValue::Tuple(vec![
			DynSolValue::Uint(round, 256),
			DynSolValue::Array(
				new_relayers.iter().map(|address| DynSolValue::Address(*address)).collect(),
			),
		])
		.abi_encode();

		let sigs = Signatures::from(self.client.sign_message(&encoded_msg).await?);
		let round_up_submit = Round_Up_Submit { round, new_relayers, sigs };

		let input = self
			.client
			.protocol_contracts
			.socket
			.round_control_poll(round_up_submit)
			.calldata()
			.clone();

		Ok(TransactionRequest::default()
			.to(*self.client.protocol_contracts.socket.address())
			.input(TransactionInput::new(input)))
	}

	/// Request send transaction to the target tx request channel.
	fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: VSPPhase1Metadata,
	) {
		send_transaction(
			self.client.clone(),
			tx_request,
			SUB_LOG_TARGET.to_string(),
			TxRequestMetadata::VSPPhase1(metadata),
			self.handle.clone(),
		);
	}

	/// Get the latest round index.
	async fn get_latest_round(&self) -> Result<U256> {
		Ok(self.client.protocol_contracts.authority.latest_round().call().await?._0)
	}
}

#[async_trait::async_trait]
impl<F, P, T> BootstrapHandler for RoundupEmitter<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		let get_next_poll_round = || async move {
			let logs = self.get_bootstrap_events().await.unwrap();

			let round_up_events: Vec<RoundUp> =
				logs.iter().map(|log| log.log_decode::<RoundUp>().unwrap().inner.data).collect();

			let max_round = round_up_events
				.iter()
				.map(|round_up_event| round_up_event.roundup.round)
				.max()
				.unwrap();
			let max_events: Vec<&RoundUp> = round_up_events
				.iter()
				.filter(|round_up_event| round_up_event.roundup.round == max_round)
				.collect();

			let status = RoundUpEventStatus::from_u8(
				max_events.iter().map(|round_up| round_up.status).max().unwrap(),
			);

			match status {
				RoundUpEventStatus::NextAuthorityRelayed => max_round,
				RoundUpEventStatus::NextAuthorityCommitted => max_round + U256::from(1),
			}
		};

		let mut next_poll_round = get_next_poll_round().await;

		loop {
			if next_poll_round == self.current_round + U256::from(1) {
				// If RoundUp reached to latest round, escape loop
				break;
			} else if next_poll_round <= self.current_round {
				// If RoundUp not reached to latest round, process round_control_poll
				if self.is_selected_relayer(next_poll_round).await? {
					let new_relayers = self.fetch_validator_list(next_poll_round).await?;
					self.request_send_transaction(
						self.build_transaction(next_poll_round, new_relayers.clone()).await?,
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
						break;
					}

					sleep(Duration::from_millis(self.client.metadata.call_interval)).await;
				}
			}
		}

		for state in self.bootstrap_shared_data.bootstrap_states.write().await.iter_mut() {
			if *state == BootstrapState::BootstrapRoundUpPhase1 {
				*state = BootstrapState::BootstrapRoundUpPhase2;
			}
		}

		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<Log>> {
		let mut round_up_events = vec![];

		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			let round_info = self.client.protocol_contracts.authority.round_info().call().await?._0;
			let bootstrap_offset_height = self
				.client
				.get_bootstrap_offset_height_based_on_block_time(
					bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
					round_info,
				)
				.await?;

			let latest_block_number = self.client.get_block_number().await?;
			let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
			let to_block = latest_block_number;

			// Split from_block into smaller chunks
			while from_block <= to_block {
				let chunk_to_block =
					std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

				let filter = Filter::new()
					.address(*self.client.protocol_contracts.socket.address())
					.event_signature(RoundUp::SIGNATURE_HASH)
					.from_block(from_block)
					.to_block(chunk_to_block);

				let chunk_logs = self.client.get_logs(&filter).await?;
				round_up_events.extend(chunk_logs);

				from_block = chunk_to_block + 1;
			}

			if round_up_events.is_empty() {
				panic!(
					"[{}]-[{}]-[{}] ❗️ Failed to find the latest RoundUp event. Please use a higher bootstrap offset. Current offset: {:?}",
					self.client.get_chain_name(),
					SUB_LOG_TARGET,
					self.client.address(),
					bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET)
				);
			}
		}

		Ok(round_up_events)
	}
}
