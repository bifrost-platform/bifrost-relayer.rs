use cccp_client::eth::{
	EthClient, EventMessage, EventMetadata, EventSender, VSPPhase1Metadata, DEFAULT_RETRIES,
};
use cccp_primitives::{
	authority_bifrost::AuthorityBifrost,
	cli::RoundupEmitterConfig,
	relayer_bifrost::RelayerManagerBifrost,
	socket_bifrost::{RoundUpSubmit, Signatures, SocketBifrost},
	sub_display_format, PeriodicWorker,
};
use cron::Schedule;
use ethers::{
	abi::{encode, Token},
	providers::{JsonRpcClient, Provider},
	types::{Address, Bytes, TransactionRequest, H160, U256},
};
use std::{str::FromStr, sync::Arc};
use tokio::time::sleep;

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
	/// Relayer_authority contract instance
	pub authority_contract: AuthorityBifrost<Provider<T>>,
	/// Socket contract(bifrost) instance
	pub socket_contract: SocketBifrost<Provider<T>>,
	/// RelayerManager contract(bifrost) instance
	pub relayer_contract: RelayerManagerBifrost<Provider<T>>,
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for RoundupEmitter<T> {
	async fn run(&mut self) {
		self.current_round =
			self.authority_contract.latest_round().call().await.unwrap_or_default();

		loop {
			self.wait_until_next_time().await;

			let latest_round =
				self.authority_contract.latest_round().call().await.unwrap_or_default();

			if self.current_round < latest_round {
				self.current_round = latest_round;

				let new_relayers = self.fetch_validator_list().await;
				self.request_send_transaction(
					self.build_transaction(latest_round, new_relayers.clone()).await,
					VSPPhase1Metadata::new(new_relayers),
				)
				.await;
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
		event_sender: Arc<EventSender>,
		client: Arc<EthClient<T>>,
		config: RoundupEmitterConfig,
	) -> Self {
		let provider = client.get_provider();

		Self {
			current_round: U256::default(),
			client,
			event_sender,
			schedule: Schedule::from_str(&config.schedule).unwrap(),
			authority_contract: AuthorityBifrost::new(
				H160::from_str(&config.authority_address).unwrap(),
				provider.clone(),
			),
			socket_contract: SocketBifrost::new(
				H160::from_str(&config.socket_address).unwrap(),
				provider.clone(),
			),
			relayer_contract: RelayerManagerBifrost::new(
				H160::from_str(&config.relayer_manager_address).unwrap(),
				provider,
			),
		}
	}

	/// Fetch new validator list
	async fn fetch_validator_list(&self) -> Vec<Address> {
		let mut addresses =
			self.relayer_contract.selected_relayers(true).call().await.unwrap_or_default();
		addresses.sort();

		addresses
	}

	/// Build `VSP phase 1` transaction.
	async fn build_transaction(
		&self,
		round: U256,
		new_relayers: Vec<Address>,
	) -> TransactionRequest {
		let data_to_sig: Bytes = encode(&[Token::Tuple(vec![
			Token::Uint(round),
			Token::Array(new_relayers.clone().into_iter().map(Token::Address).collect()),
		])])
		.into();
		let signed_sigs = self.client.wallet.sign_message(&data_to_sig);

		let sigs = Signatures {
			r: vec![signed_sigs.r.into()],
			s: vec![signed_sigs.s.into()],
			v: Bytes::from(signed_sigs.v.to_be_bytes()),
		};
		let round_up_submit = RoundUpSubmit { round, new_relayers, sigs };

		TransactionRequest::default()
			.to(self.socket_contract.address())
			.data(self.socket_contract.round_control_poll(round_up_submit).calldata().unwrap())
	}

	/// Request send transaction to the target event channel.
	async fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: VSPPhase1Metadata,
	) {
		match self.event_sender.sender.send(EventMessage::new(
			DEFAULT_RETRIES,
			tx_request,
			EventMetadata::VSPPhase1(metadata.clone()),
			false,
		)) {
			Ok(()) => log::info!(
				target: format!("{}::VSP-Phase1", &self.client.get_chain_name()).as_str(),
				"-[{}] 👤 Request VSP phase1 transaction: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata
			),
			Err(error) => log::error!(
				target: format!("{}::VSP-Phase1", &self.client.get_chain_name()).as_str(),
				"-[{}] ❗️ Failed to request VSP phase1 transaction: {}, Error: {}",
				sub_display_format(SUB_LOG_TARGET),
				metadata,
				error.to_string()
			),
		}
	}
}
