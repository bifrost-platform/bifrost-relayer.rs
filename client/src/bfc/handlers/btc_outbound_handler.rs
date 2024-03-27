use std::{collections::BTreeMap, sync::Arc, time::Duration};

use br_primitives::{
	constants::{cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET, errors::INVALID_BIFROST_NATIVENESS},
	contracts::authority::RoundMetaData,
	eth::{BootstrapState, ChainID},
	sub_display_format,
};
use ethers::{providers::JsonRpcClient, types::H256};
use subxt::events::EventDetails;
use tokio::{sync::broadcast::Receiver, time::sleep};
use tokio_stream::StreamExt;

use crate::bfc::{events::EventMessage, BfcClient, CustomConfig, UnsignedPsbtSubmitted};
use bitcoincore_rpc::bitcoin::psbt::Psbt;
use bitcoincore_rpc::bitcoin::secp256k1::All;
use br_primitives::bootstrap::BootstrapSharedData;
use subxt::backend::BlockRef;

const SUB_LOG_TARGET: &str = "regis-handler";

/// The essential task that handles `socket relay` related events.
pub struct BtcRelayHandler<T> {
	/// bfcclient
	pub bfc_client: Arc<BfcClient<T>>,
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	system_clients: BTreeMap<ChainID, Arc<BfcClient<T>>>,
	event_receiver: Receiver<EventMessage>,
}

// #[async_trait::async_trait]
impl<T: 'static + JsonRpcClient> BtcRelayHandler<T> {
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
					msg.events.len(),
				);

				let mut stream = tokio_stream::iter(msg.events);

				while let Some(ext_events) = stream.next().await {
					self.process_confirmed_event(&ext_events, false).await;
					// if self.is_target_contract(&ext_events) && self.is_target_event(&ext_events) {
					// }
				}
			}
		}
	}

	fn is_target_contract(&self, ext_events: &EventDetails<CustomConfig>) -> bool {
		true
	}

	fn is_target_event(&self, topic: H256) -> bool {
		true
	}

	async fn process_confirmed_event(
		&self,
		ext_events: &EventDetails<CustomConfig>,
		is_bootstrap: bool,
	) {
		let matching_event = ext_events.as_event::<UnsignedPsbtSubmitted>().unwrap();

		if matching_event.is_none() {
			return;
		}

		let matching_event_psbt = matching_event.unwrap().psbt;

		match Psbt::deserialize(&matching_event_psbt) {
			Ok(deserialized_psbt) => {
				if !is_bootstrap {
					log::info!(
						target: &self.bfc_client
						.eth_client.get_chain_name(),
						"-[{}] 👤 psbt event detected. ({:?})",
						sub_display_format(SUB_LOG_TARGET),
						&matching_event_psbt,
					);
				}
				if (!self.is_selected_relayer().await) & (!self.is_selected_socket().await) {
					// do nothing if not selected
					return;
				}
				self.bfc_client
					.submit_signed_psbt::<All>(
						self.bfc_client.eth_client.address(),
						deserialized_psbt,
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
					&matching_event_psbt,
					e.to_string(),
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] Error on decoding RoundUp event ({:?}):{}",
						&self.bfc_client.eth_client.get_chain_name(),
						SUB_LOG_TARGET,
						self.bfc_client.eth_client.address(),
						&matching_event_psbt,
						e
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self) -> bool {
		let relayer_manager =
			self.bfc_client.eth_client.protocol_contracts.relayer_manager.as_ref().unwrap();

		let round = self
			.bfc_client
			.eth_client
			.contract_call(relayer_manager.latest_round(), "relayer_manager.latest_round")
			.await;
		self.bfc_client
			.eth_client
			.contract_call(
				relayer_manager.is_previous_selected_relayer(
					round,
					self.bfc_client.eth_client.address(),
					false,
				),
				"relayer_manager.is_previous_selected_relayer",
			)
			.await
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_socket(&self) -> bool {
		true
	}

	async fn bootstrap(&self) {
		log::info!(
			target: &self.bfc_client.eth_client.get_chain_name(),
			"-[{}] ⚙️  [Bootstrap mode] Bootstrapping Socket events.",
			sub_display_format(SUB_LOG_TARGET),
		);

		let events = self.get_bootstrap_events().await;

		let mut stream = tokio_stream::iter(events);
		while let Some(log) = stream.next().await {
			self.process_confirmed_event(&log, true).await;
		}

		let mut bootstrap_count = self.bootstrap_shared_data.socket_bootstrap_count.lock().await;
		*bootstrap_count += 1;

		// If All thread complete the task, starts the blockManager
		if *bootstrap_count == self.system_clients.len() as u8 {
			let mut bootstrap_guard = self.bootstrap_shared_data.bootstrap_states.write().await;

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

	async fn get_bootstrap_events(&self) -> Vec<EventDetails<CustomConfig>> {
		let mut events = vec![];

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
				let chunk_to_block = std::cmp::min(from_block + 1, to_block);

				let block_hash = self
					.bfc_client
					.eth_client
					.get_block(chunk_to_block.into())
					.await
					.unwrap()
					.hash
					.unwrap();

				let target_block_events = self
					.bfc_client
					.client
					.blocks()
					.at(BlockRef::from_hash(block_hash))
					.await
					.unwrap()
					.events()
					.await
					.unwrap();

				events.extend(target_block_events.iter().filter_map(Result::ok));

				from_block = chunk_to_block;
			}
		}

		events
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
