use std::{collections::BTreeMap, str::FromStr, sync::Arc};

use br_client::eth::{
	EthClient, EventMessage, EventMetadata, EventSender, RollbackMetadata, SocketRelayBuilder,
	TxRequest,
};
use br_primitives::{
	eth::{ChainID, GasCoefficient, SocketEventStatus},
	socket::{PollSubmit, Signatures, SocketMessage},
	sub_display_format, PeriodicWorker, RawRequestID, RollbackableMessage, INVALID_CHAIN_ID,
	INVALID_PERIODIC_SCHEDULE, ROLLBACK_CHECK_MINIMUM_INTERVAL, ROLLBACK_CHECK_SCHEDULE,
};
use cron::Schedule;
use ethers::{
	providers::JsonRpcClient,
	types::{TransactionRequest, U256},
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

const SUB_LOG_TARGET: &str = "rollback-handler";

/// The essential task that handles `Socket` event rollbacks.
/// This only handles requests that are relayed to the target client.
/// (`client` and `event_sender` are connected to the same chain)
pub struct SocketRollbackHandler<T> {
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<T>>,
	/// The receiver connected to the socket rollback channel.
	rollback_receiver: UnboundedReceiver<SocketMessage>,
	/// The local storage saving emitted `Socket` event messages.
	rollback_msgs: BTreeMap<RawRequestID, RollbackableMessage>,
	/// The event senders that sends messages to the event channel.
	event_sender: Arc<EventSender>,
	/// The time schedule that represents when to check heartbeat pulsed.
	schedule: Schedule,
}

impl<T: JsonRpcClient> SocketRollbackHandler<T> {
	/// Instantiates a new `SocketRollbackHandler`.
	pub fn new(
		event_sender: Arc<EventSender>,
		system_clients: Vec<Arc<EthClient<T>>>,
	) -> (Self, UnboundedSender<SocketMessage>) {
		let (sender, rollback_receiver) = mpsc::unbounded_channel::<SocketMessage>();

		(
			Self {
				client: system_clients
					.iter()
					.find(|client| client.get_chain_id() == event_sender.id)
					.expect(INVALID_CHAIN_ID)
					.clone(),
				rollback_receiver,
				rollback_msgs: BTreeMap::new(),
				event_sender,
				schedule: Schedule::from_str(ROLLBACK_CHECK_SCHEDULE)
					.expect(INVALID_PERIODIC_SCHEDULE),
			},
			sender,
		)
	}

	/// Verifies whether the given socket message has been executed or reverted.
	async fn is_request_executed(&self, socket_msg: &SocketMessage) -> bool {
		let request = self
			.client
			.contract_call(
				self.client.protocol_contracts.socket.get_request(socket_msg.req_id.clone()),
				"socket.get_request",
			)
			.await;

		// If the state has changed, we consider that it has been executed (or rejected).
		// `socket_msg.status` will either be `Inbound::Requested` or `Outbound::Accepted`.
		if SocketEventStatus::from_u8(socket_msg.status)
			!= SocketEventStatus::from_u8(request.field[0].clone().into())
		{
			return true;
		}
		false
	}

	/// Verifies whether a certain socket message has been waited for at least the required minimum time.
	fn is_interval_passed(&self, requested_timestamp: U256, current_timestamp: U256) -> bool {
		if current_timestamp.saturating_sub(requested_timestamp)
			>= ROLLBACK_CHECK_MINIMUM_INTERVAL.into()
		{
			return true;
		}
		false
	}

	/// Tries to rollback the given socket message.
	async fn try_rollback(&self, socket_msg: &SocketMessage) {
		let status = SocketEventStatus::from_u8(socket_msg.status);
		match status {
			SocketEventStatus::Requested => self.try_rollback_inbound(socket_msg).await,
			SocketEventStatus::Accepted => self.try_rollback_outbound(socket_msg).await,
			_ => panic!("Trying rollback on an invalid socket event status"),
		}
	}

	/// Tries to rollback the given inbound socket message.
	async fn try_rollback_inbound(&self, socket_msg: &SocketMessage) {
		let mut submit_sig = socket_msg.clone();
		submit_sig.status = SocketEventStatus::Failed.into();
		let poll_submit = PollSubmit {
			msg: submit_sig.clone(),
			sigs: Signatures::default(),
			option: U256::default(),
		};
		let call_data = self.client.protocol_contracts.socket.poll(poll_submit).calldata().unwrap();
		let tx_request = TransactionRequest::default().data(call_data).to(self
			.client
			.protocol_contracts
			.socket
			.address());

		let metadata = RollbackMetadata::new(
			true,
			SocketEventStatus::Failed,
			socket_msg.req_id.sequence,
			ChainID::from_be_bytes(socket_msg.req_id.chain),
			ChainID::from_be_bytes(socket_msg.ins_code.chain),
		);

		self.request_send_transaction(tx_request, metadata, false, GasCoefficient::Mid);
	}

	/// Tries to rollback the given outbound socket message.
	async fn try_rollback_outbound(&self, socket_msg: &SocketMessage) {
		// `submit_sig` is the state changed socket message that will be passed.
		// `socket_msg` is the origin message that will be used for signature builds.
		let mut submit_sig = socket_msg.clone();
		submit_sig.status = SocketEventStatus::Rejected.into();
		let tx_request = TransactionRequest::default()
			.data(self.build_poll_call_data(
				submit_sig.clone(),
				self.get_sorted_signatures(socket_msg.clone()).await,
			))
			.to(self.client.protocol_contracts.socket.address());

		let metadata = RollbackMetadata::new(
			false,
			SocketEventStatus::Rejected,
			socket_msg.req_id.sequence,
			ChainID::from_be_bytes(socket_msg.req_id.chain),
			ChainID::from_be_bytes(socket_msg.ins_code.chain),
		);

		self.request_send_transaction(tx_request, metadata, true, GasCoefficient::Low);
	}

	/// Tries to receive any new rollbackable messages and store's it locally.
	fn receive(&mut self, current_timestamp: U256) {
		while let Ok(msg) = self.rollback_receiver.try_recv() {
			let req_id = msg.req_id.sequence;
			if self.rollback_msgs.contains_key(&req_id) {
				continue;
			}
			self.rollback_msgs
				.insert(req_id, RollbackableMessage::new(current_timestamp, msg));
		}
	}

	/// Request a socket rollback transaction to the target event channel.
	fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: RollbackMetadata,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		match self.event_sender.send(EventMessage::new(
			TxRequest::Legacy(tx_request),
			EventMetadata::Rollback(metadata.clone()),
			true,
			give_random_delay,
			gas_coefficient,
			false,
		)) {
			Ok(()) => {
				log::info!(
					target: &self.client.get_chain_name(),
					"-[{}] 🔃 Try Rollback::Socket: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata
				);
			},
			Err(error) => {
				log::error!(
					target: &self.client.get_chain_name(),
					"-[{}] ❗️ Failed to try Rollback::Socket: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					metadata,
					error.to_string()
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] ❗️ Failed to try Rollback::Socket: {}, Error: {}",
						&self.client.get_chain_name(),
						SUB_LOG_TARGET,
						self.client.address(),
						metadata,
						error
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> SocketRelayBuilder<T> for SocketRollbackHandler<T> {
	fn get_client(&self) -> Arc<EthClient<T>> {
		self.client.clone()
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for SocketRollbackHandler<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			// executed or rollback handled request ID's.
			let mut handled_req_ids = vec![];

			if let Some(latest_block) =
				self.client.get_block(self.client.get_latest_block_number().await.into()).await
			{
				self.receive(latest_block.timestamp);

				for (req_id, rollback_msg) in self.rollback_msgs.clone() {
					if !self.is_interval_passed(rollback_msg.timestamp, latest_block.timestamp) {
						continue;
					}
					if !self.is_request_executed(&rollback_msg.socket_msg).await {
						// the pending request has not been processed through the schedule. rollback should be handled.
						self.try_rollback(&rollback_msg.socket_msg).await;
					}
					handled_req_ids.push(req_id);
				}
				for req_id in handled_req_ids {
					self.rollback_msgs.remove(&req_id);
				}
			}

			self.wait_until_next_time().await;
		}
	}
}
