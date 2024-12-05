use alloy::{
	consensus::BlockHeader as _,
	primitives::ChainId,
	providers::{fillers::TxFiller, Provider, WalletProvider},
	rpc::types::TransactionRequest,
	transports::Transport,
};
use byteorder::{BigEndian, ByteOrder as _};
use cron::Schedule;
use eyre::Result;
use std::{collections::BTreeMap, str::FromStr, sync::Arc};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use br_client::eth::{traits::SocketRelayBuilder, EthClient};
use br_primitives::{
	constants::{
		errors::{INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_PERIODIC_SCHEDULE},
		schedule::{ROLLBACK_CHECK_MINIMUM_INTERVAL, ROLLBACK_CHECK_SCHEDULE},
	},
	contracts::socket::Socket_Struct::{RequestID, RequestInfo, Signatures, Socket_Message},
	eth::{GasCoefficient, RelayDirection, SocketEventStatus},
	periodic::{RawRequestID, RollbackableMessage},
	tx::{RollbackMetadata, TxRequestMessage, TxRequestMetadata, TxRequestSender},
	utils::sub_display_format,
};

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "rollback-emitter";

/// The essential task that handles `Socket` event rollbacks.
/// This only handles requests that are relayed to the target client.
/// (`client` and `tx_request_sender` are connected to the same chain)
pub struct SocketRollbackEmitter<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// The `EthClient` to interact with the connected blockchain.
	pub client: Arc<EthClient<F, P, T>>,
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	system_clients: Arc<BTreeMap<ChainId, Arc<EthClient<F, P, T>>>>,
	/// The receiver connected to the socket rollback channel.
	rollback_receiver: UnboundedReceiver<Socket_Message>,
	/// The local storage saving emitted `Socket` event messages.
	rollback_msgs: BTreeMap<RawRequestID, RollbackableMessage>,
	/// The sender that sends messages to the tx request channel.
	tx_request_sender: Arc<TxRequestSender>,
	/// The time schedule that represents when to check heartbeat pulsed.
	schedule: Schedule,
}

impl<F, P, T> SocketRollbackEmitter<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// Instantiates a new `SocketRollbackEmitter`.
	pub fn new(
		tx_request_sender: Arc<TxRequestSender>,
		system_clients: Arc<BTreeMap<ChainId, Arc<EthClient<F, P, T>>>>,
	) -> (Self, UnboundedSender<Socket_Message>) {
		let (sender, rollback_receiver) = mpsc::unbounded_channel::<Socket_Message>();

		(
			Self {
				client: system_clients.get(&tx_request_sender.id).expect(INVALID_CHAIN_ID).clone(),
				system_clients,
				rollback_receiver,
				rollback_msgs: BTreeMap::new(),
				tx_request_sender,
				schedule: Schedule::from_str(ROLLBACK_CHECK_SCHEDULE)
					.expect(INVALID_PERIODIC_SCHEDULE),
			},
			sender,
		)
	}

	/// Verifies whether the given socket message has been executed.
	async fn is_request_executed(&self, socket_msg: &Socket_Message) -> Result<bool> {
		let src_request = self
			.get_socket_request(
				&socket_msg.req_id,
				BigEndian::read_u32(&socket_msg.req_id.ChainIndex.0) as ChainId,
			)
			.await?;
		let dst_request = self
			.get_socket_request(
				&socket_msg.req_id,
				BigEndian::read_u32(&socket_msg.ins_code.ChainIndex.0) as ChainId,
			)
			.await?;

		if let (Some(src_request), Some(dst_request)) = (src_request, dst_request) {
			let src_status = SocketEventStatus::from(&src_request.field[0]);
			let dst_status = SocketEventStatus::from(&dst_request.field[0]);

			match src_status {
				SocketEventStatus::Committed | SocketEventStatus::Rollbacked => return Ok(true),
				_ => (),
			}
			if self.is_inbound_sequence(
				BigEndian::read_u32(&socket_msg.ins_code.ChainIndex.0) as ChainId
			) {
				match dst_status {
					SocketEventStatus::Executed
					| SocketEventStatus::Reverted
					| SocketEventStatus::Accepted
					| SocketEventStatus::Rejected => return Ok(true),
					_ => (),
				}
			} else {
				match dst_status {
					SocketEventStatus::Executed | SocketEventStatus::Reverted => return Ok(true),
					_ => (),
				}
			}
		}
		Ok(false)
	}

	/// Verifies whether the socket event is an inbound sequence.
	fn is_inbound_sequence(&self, dst_chain_id: ChainId) -> bool {
		if let Some(client) = self.system_clients.get(&dst_chain_id) {
			return matches!(client.metadata.if_destination_chain, RelayDirection::Inbound);
		}
		false
	}

	/// Verifies whether a certain socket message has been waited for at least the required minimum time.
	fn is_request_timeout(&self, timeout_started_at: u64, current_timestamp: u64) -> bool {
		if current_timestamp.saturating_sub(timeout_started_at) >= ROLLBACK_CHECK_MINIMUM_INTERVAL {
			return true;
		}
		false
	}

	/// Get the current state of the socket request on the target chain.
	async fn get_socket_request(
		&self,
		req_id: &RequestID,
		chain_id: ChainId,
	) -> Result<Option<RequestInfo>> {
		if let Some(client) = self.system_clients.get(&chain_id) {
			return Ok(Some(
				client.protocol_contracts.socket.get_request(req_id.clone()).call().await?._0,
			));
		}
		Ok(None)
	}

	/// Tries to rollback the given socket message.
	async fn try_rollback(&self, socket_msg: &Socket_Message) -> Result<()> {
		let status = SocketEventStatus::from(socket_msg.status);
		match status {
			SocketEventStatus::Requested => self.try_rollback_inbound(socket_msg).await,
			SocketEventStatus::Accepted => self.try_rollback_outbound(socket_msg).await?,
			_ => panic!("Trying rollback on an invalid socket event status"),
		}

		Ok(())
	}

	/// Tries to rollback the given inbound socket message.
	async fn try_rollback_inbound(&self, socket_msg: &Socket_Message) {
		let mut submit_sig = socket_msg.clone();
		submit_sig.status = SocketEventStatus::Failed.into();
		let tx_request = TransactionRequest::default()
			.input(self.build_poll_call_data(submit_sig.clone(), Signatures::default()))
			.to(*self.client.protocol_contracts.socket.address());

		let metadata = RollbackMetadata::new(
			true,
			SocketEventStatus::Failed,
			socket_msg.req_id.sequence,
			BigEndian::read_u32(&socket_msg.req_id.ChainIndex.0) as ChainId,
			BigEndian::read_u32(&socket_msg.ins_code.ChainIndex.0) as ChainId,
		);

		// transaction executed on Bifrost so no random delay required.
		// due to majority checks, higher gas coefficient required.
		self.request_send_transaction(tx_request, metadata, false, GasCoefficient::Mid);
	}

	/// Tries to rollback the given outbound socket message.
	async fn try_rollback_outbound(&self, socket_msg: &Socket_Message) -> Result<()> {
		// `submit_sig` is the state changed socket message that will be passed.
		// `socket_msg` is the origin message that will be used for signature builds.
		let mut submit_sig = socket_msg.clone();
		submit_sig.status = SocketEventStatus::Rejected.into();
		let tx_request = TransactionRequest::default()
			.input(self.build_poll_call_data(
				submit_sig.clone(),
				self.get_sorted_signatures(socket_msg.clone()).await?,
			))
			.to(*self.client.protocol_contracts.socket.address());

		let metadata = RollbackMetadata::new(
			false,
			SocketEventStatus::Rejected,
			socket_msg.req_id.sequence,
			BigEndian::read_u32(&socket_msg.req_id.ChainIndex.0) as ChainId,
			BigEndian::read_u32(&socket_msg.ins_code.ChainIndex.0) as ChainId,
		);

		// transaction executed on External chain's so random delay required.
		// aggregated relay typed transactions are good with low gas coefficient.
		self.request_send_transaction(tx_request, metadata, true, GasCoefficient::Low);

		Ok(())
	}

	/// Tries to receive any new rollbackable messages and store's it locally.
	/// The timestamp will be set to the current highest block's timestamp.
	fn receive(&mut self, current_timestamp: u64) {
		while let Ok(msg) = self.rollback_receiver.try_recv() {
			// prevent rollback for bitcoin bridges
			if let Some(bitcoin_chain_id) = self.client.get_bitcoin_chain_id() {
				if BigEndian::read_u32(&msg.req_id.ChainIndex.0) as u64 == bitcoin_chain_id
					|| BigEndian::read_u32(&msg.ins_code.ChainIndex.0) as u64 == bitcoin_chain_id
				{
					continue;
				}
			}

			let req_id = msg.req_id.sequence;
			// ignore if the request already exists.
			if self.rollback_msgs.contains_key(&req_id) {
				continue;
			}
			self.rollback_msgs
				.insert(req_id, RollbackableMessage::new(current_timestamp, msg));

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔃 Received Rollbackable Socket message: {}",
				sub_display_format(SUB_LOG_TARGET),
				req_id,
			);
		}
	}

	/// Request a socket rollback transaction to the target tx request channel.
	fn request_send_transaction(
		&self,
		tx_request: TransactionRequest,
		metadata: RollbackMetadata,
		give_random_delay: bool,
		gas_coefficient: GasCoefficient,
	) {
		// asynchronous transaction tasks will work fine for rollback transactions,
		// so `is_bootstrap` parameter is set to `false`.
		match self.tx_request_sender.send(TxRequestMessage::new(
			tx_request,
			TxRequestMetadata::Rollback(metadata.clone()),
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
				let log_msg = format!(
					"-[{}]-[{}] ❗️ Failed to try Rollback::Socket: {}, Error: {}",
					sub_display_format(SUB_LOG_TARGET),
					self.client.address(),
					metadata,
					error
				);
				log::error!(target: &self.client.get_chain_name(), "{log_msg}");
				sentry::capture_message(
					&format!("[{}]{log_msg}", &self.client.get_chain_name()),
					sentry::Level::Error,
				);
			},
		}
	}
}

#[async_trait::async_trait]
impl<F, P, T> SocketRelayBuilder<F, P, T> for SocketRollbackEmitter<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	fn get_client(&self) -> Arc<EthClient<F, P, T>> {
		// This will always return the Bifrost client.
		// Used only for `get_sorted_signatures()` on `Outbound::Accepted` rollbacks.
		self.system_clients
			.iter()
			.find(|(_id, client)| client.metadata.is_native)
			.expect(INVALID_BIFROST_NATIVENESS)
			.1
			.clone()
	}
}

#[async_trait::async_trait]
impl<F, P, T> PeriodicWorker for SocketRollbackEmitter<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		loop {
			self.wait_until_next_time().await;

			// executed or rollback handled request ID's.
			let mut handled_req_ids = vec![];

			if let Some(latest_block) = self
				.client
				.get_block(self.client.get_block_number().await?.into(), true.into())
				.await?
			{
				self.receive(latest_block.header.timestamp());

				for (req_id, rollback_msg) in self.rollback_msgs.clone() {
					// ignore if the request has already been processed.
					// it should be removed from the local storage.
					if self.is_request_executed(&rollback_msg.socket_msg).await? {
						handled_req_ids.push(req_id);
						continue;
					}
					// ignore if the required interval didn't pass.
					if !self.is_request_timeout(
						rollback_msg.timeout_started_at,
						latest_block.header.timestamp(),
					) {
						continue;
					}
					// the pending request has not been processed in the waiting period. rollback should be handled.
					self.try_rollback(&rollback_msg.socket_msg).await;
					handled_req_ids.push(req_id);
				}
			}
			for req_id in handled_req_ids {
				self.rollback_msgs.remove(&req_id);
			}

			log::info!(
				target: &self.client.get_chain_name(),
				"-[{}] 🔃 Checked Rollbackable::Socket entries: {}",
				sub_display_format(SUB_LOG_TARGET),
				self.rollback_msgs.len()
			);
		}
	}
}
