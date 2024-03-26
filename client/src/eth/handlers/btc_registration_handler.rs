use std::{collections::BTreeMap, sync::Arc, time::Duration};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	constants::{
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET,
		config::BOOTSTRAP_BLOCK_CHUNK_SIZE,
		errors::{INVALID_BIFROST_NATIVENESS, INVALID_CHAIN_ID, INVALID_CONTRACT_ABI},
	},
	contracts::{
		authority::RoundMetaData,
		btc_registration::{BtcRegisEvents, VaultPending},
		socket::{RequestID, Signatures, SocketEvents, SocketMessage},
	},
	eth::{
		BootstrapState, BuiltRelayTransaction, ChainID, GasCoefficient, RelayDirection,
		SocketEventStatus,
	},
	sub_display_format,
};
use ethers::{
	abi::{Detokenize, Tokenize},
	contract::EthLogDecode,
	providers::JsonRpcClient,
	types::{Filter, Log, TransactionRequest, H256, U256},
};

use tokio::{sync::broadcast::Receiver, time::sleep};
use tokio_stream::StreamExt;

use crate::bfc::BfcClient;

use crate::eth::{
	events::EventMessage,
	traits::{BootstrapHandler, Handler, SocketRelayBuilder},
	EthClient,
};

const SUB_LOG_TARGET: &str = "regis-handler";

/// The essential task that handles `socket relay` related events.
pub struct RegisHandler<T> {
	/// bfcclient
	pub bfc_client: Arc<BfcClient<T>>,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// The receiver that consumes new events from the block channel.
	event_receiver: Receiver<EventMessage>,
	/// Signature of the `Socket` Event.
	socket_signature: H256,
	/// The entire clients instantiated in the system. <chain_id, Arc<BfcClient>>
	system_clients: BTreeMap<ChainID, Arc<BfcClient<T>>>,
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> Handler for RegisHandler<T> {
	async fn run(&mut self) {
		loop {
			if self.is_bootstrap_state_synced_as(BootstrapState::BootstrapSocketRelay).await {
				self.bootstrap().await;

				sleep(Duration::from_millis(self.bfc_client.eth_client.metadata.call_interval))
					.await;
			} else if self.is_bootstrap_state_synced_as(BootstrapState::NormalStart).await {
				let msg = self.event_receiver.recv().await.unwrap();

				log::info!(
					target: &self.bfc_client.eth_client.get_chain_name(),
					"-[{}] 📦 Imported #{:?} with target logs({:?})",
					sub_display_format(SUB_LOG_TARGET),
					msg.block_number,
					msg.event_logs.len(),
				);

				let mut stream = tokio_stream::iter(msg.event_logs);
				while let Some(log) = stream.next().await {
					if self.is_target_contract(&log) && self.is_target_event(log.topics[0]) {
						self.process_confirmed_log(&log, false).await;
					}
				}
			}
		}
	}

	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool) {
		if let Some(receipt) = self
			.bfc_client
			.eth_client
			.get_transaction_receipt(log.transaction_hash.unwrap())
			.await
		{
			if receipt.status.unwrap().is_zero() {
				return;
			}
			match self.decode_log(log.clone()).await {
				Ok(serialized_log) => {
					if !is_bootstrap {
						log::info!(
							target: &self.bfc_client
							.eth_client.get_chain_name(),
							"-[{}] 👤 registration event detected. ({:?}-{:?})",
							sub_display_format(SUB_LOG_TARGET),
							serialized_log.user_bfc_address,
							log.transaction_hash,
						);
					}
					if !self.is_selected_relayer().await {
						// do nothing if not selected
						return;
					}
					self.bfc_client
						.submit_vault_key(
							self.bfc_client.eth_client.address(),
							serialized_log.user_bfc_address,
						)
						.await
						.unwrap();
				},
				Err(e) => {
					log::error!(
						target: &self.bfc_client
							.eth_client.get_chain_name(),
						"-[{}] Error on decoding RoundUp event ({:?}):{}",
						sub_display_format(SUB_LOG_TARGET),
						log.transaction_hash,
						e.to_string(),
					);
					sentry::capture_message(
						format!(
							"[{}]-[{}]-[{}] Error on decoding RoundUp event ({:?}):{}",
							&self.bfc_client.eth_client.get_chain_name(),
							SUB_LOG_TARGET,
							self.bfc_client.eth_client.address(),
							log.transaction_hash,
							e
						)
						.as_str(),
						sentry::Level::Error,
					);
				},
			}
		}
	}

	fn is_target_contract(&self, log: &Log) -> bool {
		if log.address == self.bfc_client.eth_client.protocol_contracts.socket.address() {
			return true;
		}
		false
	}

	fn is_target_event(&self, topic: H256) -> bool {
		topic == self.socket_signature
	}
}

#[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> BootstrapHandler for RegisHandler<T> {
	async fn bootstrap(&self) {
		log::info!(
			target: &self.bfc_client.eth_client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping Socket events.",
			sub_display_format(SUB_LOG_TARGET),
		);

		let logs = self.get_bootstrap_events().await;

		let mut stream = tokio_stream::iter(logs);
		while let Some(log) = stream.next().await {
			self.process_confirmed_log(&log, true).await;
		}

		let mut bootstrap_count = self.bootstrap_shared_data.socket_bootstrap_count.lock().await;
		*bootstrap_count += 1;

		// If All thread complete the task, starts the blockManager
		if *bootstrap_count == self.system_clients.len() as u8 {
			let mut bootstrap_guard: tokio::sync::RwLockWriteGuard<'_, Vec<BootstrapState>> =
				self.bootstrap_shared_data.bootstrap_states.write().await;

			for state in bootstrap_guard.iter_mut() {
				*state = BootstrapState::NormalStart;
			}

			log::info!(
				target: "bifrost-relayer",
				"-[{}] ⚙️  [Bootstrap mode] Bootstrap process successfully ended.",
				sub_display_format(SUB_LOG_TARGET),
			);
		}
	}

	async fn get_bootstrap_events(&self) -> Vec<Log> {
		let mut logs = vec![];

		if let Some(bootstrap_config) = &self.bootstrap_shared_data.bootstrap_config {
			let round_info: RoundMetaData = if self.bfc_client.eth_client.metadata.is_native {
				self.bfc_client
					.eth_client
					.contract_call(
						self.bfc_client.eth_client.protocol_contracts.authority.round_info(),
						"authority.round_info",
					)
					.await
			} else if let Some((_id, native_client)) = self
				.system_clients
				.iter()
				.find(|(_id, bfc_client)| bfc_client.eth_client.metadata.is_native)
			{
				native_client
					.eth_client
					.contract_call(
						native_client.eth_client.protocol_contracts.authority.round_info(),
						"authority.round_info",
					)
					.await
			} else {
				panic!(
					"[{}]-[{}] {}",
					self.bfc_client.eth_client.get_chain_name(),
					SUB_LOG_TARGET,
					INVALID_BIFROST_NATIVENESS,
				);
			};

			let bootstrap_offset_height = self
				.bfc_client
				.eth_client
				.get_bootstrap_offset_height_based_on_block_time(
					bootstrap_config.round_offset.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
					round_info,
				)
				.await;

			let latest_block_number = self.bfc_client.eth_client.get_latest_block_number().await;
			let mut from_block = latest_block_number.saturating_sub(bootstrap_offset_height);
			let to_block = latest_block_number;

			// Split from_block into smaller chunks
			while from_block <= to_block {
				let chunk_to_block =
					std::cmp::min(from_block + BOOTSTRAP_BLOCK_CHUNK_SIZE - 1, to_block);

				let filter = Filter::new()
					.address(self.bfc_client.eth_client.protocol_contracts.socket.address())
					.topic0(self.socket_signature)
					.from_block(from_block)
					.to_block(chunk_to_block);
				let target_logs_chunk = self.bfc_client.eth_client.get_logs(&filter).await;
				logs.extend(target_logs_chunk);

				from_block = chunk_to_block + 1;
			}
		}

		logs
	}

	async fn is_bootstrap_state_synced_as(&self, state: BootstrapState) -> bool {
		self.bootstrap_shared_data
			.bootstrap_states
			.read()
			.await
			.iter()
			.all(|s| *s == state)
	}
}

impl<T: JsonRpcClient> RegisHandler<T> {
	/// Decode & Serialize log to `Serialized` struct.
	async fn decode_log(&self, log: Log) -> Result<VaultPending, ethers::abi::Error> {
		match BtcRegisEvents::decode_log(&log.into()) {
			Ok(regis) => Ok(VaultPending::from_tokens(regis.into_tokens()).unwrap()),
			Err(error) => Err(error),
		}
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self) -> bool {
		true
	}
}
