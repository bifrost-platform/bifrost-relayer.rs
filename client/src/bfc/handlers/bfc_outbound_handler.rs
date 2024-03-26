use std::{collections::BTreeMap, sync::Arc, time::Duration};

use br_primitives::{
	constants::{
		cli::DEFAULT_BOOTSTRAP_ROUND_OFFSET, config::BOOTSTRAP_BLOCK_CHUNK_SIZE,
		errors::INVALID_BIFROST_NATIVENESS,
	},
	contracts::{
		authority::RoundMetaData,
		btc_registration::{BtcRegisEvents, VaultPending},
		btc_scoket_queue::{
			BtcSocketEvents,
			RequestFinalized,
			// SignedPsbtSubmitted, UnsignedPsbtSubmitted,
		},
	},
	eth::{BootstrapState, ChainID},
	sub_display_format,
};
use ethers::{
	abi::{Detokenize, Tokenize},
	contract::EthLogDecode,
	providers::JsonRpcClient,
	types::Log,
};
use subxt::events::EventDetails;
use tokio::{sync::broadcast::Receiver, time::sleep};
use tokio_stream::StreamExt;

use crate::bfc::{BfcClient, CustomConfig, SignedPsbtSubmitted, UnsignedPsbtSubmitted};
use bitcoincore_rpc::bitcoin::psbt::Psbt;
use br_primitives::bootstrap::BootstrapSharedData;
use subxt::backend::BlockRef;
use subxt::error::Error;
use subxt::events::Events;

const SUB_LOG_TARGET: &str = "regis-handler";

/// The essential task that handles `socket relay` related events.
pub struct BtcRelayHandler<T> {
	/// bfcclient
	pub bfc_client: Arc<BfcClient<T>>,
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	system_clients: BTreeMap<ChainID, Arc<BfcClient<T>>>,
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
				let events = self.get_bootstrap_events().await;
				let mut stream = tokio_stream::iter(events);

				while let Some(ext_events) = stream.next().await {
					self.process_confirmed_event(&ext_events, false).await;
				}
			}
		}
	}

	async fn process_confirmed_event(
		&self,
		ext_events: &EventDetails<CustomConfig>,
		is_bootstrap: bool,
	) {
		let matching_event = ext_events.as_event::<UnsignedPsbtSubmitted>();

		if matching_event.is_none() {
			return;
		}

		match Psbt::deserialize(&matching_event.psbt) {
			Ok(deserialized_psbt) => {
				if !is_bootstrap {
					log::info!(
						target: &self.bfc_client
						.eth_client.get_chain_name(),
						"-[{}] 👤 psbt event detected. ({:?}-{:?})",
						sub_display_format(SUB_LOG_TARGET),
						// deserialized_psbt.req_id,
						// log.transaction_hash,
					);
				}
				if (!self.is_selected_relayer().await) & (!self.is_selected_socket().await) {
					// do nothing if not selected
					return;
				}
				self.bfc_client.submit_signed_psbt(deserialized_psbt).await.unwrap();
			},
			Err(e) => {
				log::error!(
					target: &self.bfc_client
						.eth_client.get_chain_name(),
					"-[{}] Error on decoding RoundUp event ({:?}):{}",
					sub_display_format(SUB_LOG_TARGET),
					// log.transaction_hash,
					// e.to_string(),
				);
				sentry::capture_message(
					format!(
						"[{}]-[{}]-[{}] Error on decoding RoundUp event ({:?}):{}",
						&self.bfc_client.eth_client.get_chain_name(),
						SUB_LOG_TARGET,
						self.bfc_client.eth_client.address(),
						// log.transaction_hash,
						// e
					)
					.as_str(),
					sentry::Level::Error,
				);
			},
		}
	}

	// async fn process_confirmed_log(&self, ext_events: &Events<CustomConfig>, is_bootstrap: bool) {
	// 	let matching_event = ext_events
	// 		.iter()
	// 		.flat_map(|events| {
	// 			events.iter().map(|x| x.and_then(|y| y.as_event::<UnsignedPsbtSubmitted>()))
	// 		})
	// 		.next();

	// 	if matching_event.is_none() {
	// 		return;
	// 	}

	// 	match Psbt::deserialize(&matching_event.psbt) {
	// 		Ok(deserialized_psbt) => {
	// 			if !is_bootstrap {
	// 				log::info!(
	// 					target: &self.bfc_client
	// 					.eth_client.get_chain_name(),
	// 					"-[{}] 👤 psbt event detected. ({:?}-{:?})",
	// 					sub_display_format(SUB_LOG_TARGET),
	// 					// deserialized_psbt.req_id,
	// 					// log.transaction_hash,
	// 				);
	// 			}
	// 			if (!self.is_selected_relayer().await) & (!self.is_selected_socket().await) {
	// 				// do nothing if not selected
	// 				return;
	// 			}
	// 			self.bfc_client.submit_signed_psbt(deserialized_psbt).await.unwrap();
	// 		},
	// 		Err(e) => {
	// 			log::error!(
	// 				target: &self.bfc_client
	// 					.eth_client.get_chain_name(),
	// 				"-[{}] Error on decoding RoundUp event ({:?}):{}",
	// 				sub_display_format(SUB_LOG_TARGET),
	// 				// log.transaction_hash,
	// 				// e.to_string(),
	// 			);
	// 			sentry::capture_message(
	// 				format!(
	// 					"[{}]-[{}]-[{}] Error on decoding RoundUp event ({:?}):{}",
	// 					&self.bfc_client.eth_client.get_chain_name(),
	// 					SUB_LOG_TARGET,
	// 					self.bfc_client.eth_client.address(),
	// 					// log.transaction_hash,
	// 					// e
	// 				)
	// 				.as_str(),
	// 				sentry::Level::Error,
	// 			);
	// 		},
	// 	}
	// }

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_relayer(&self) -> bool {
		true
	}

	/// Verifies whether the current relayer was selected at the given round.
	async fn is_selected_socket(&self) -> bool {
		true
	}

	// async fn subscribe_events(
	// 	&self,
	// ) -> Result<impl Stream<Item = Result<Events<CustomConfig>, Error>> + Unpin, Error> {
	// 	Ok(self
	// 		.bfc_client
	// 		.client
	// 		.blocks()
	// 		.subscribe_best()
	// 		.await?
	// 		.then(|x| async move { x?.events().await })
	// 		.boxed())
	// }

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
