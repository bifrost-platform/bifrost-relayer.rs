use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

use alloy::{
	network::Network,
	primitives::ChainId,
	providers::{Provider, WalletProvider, fillers::TxFiller},
	sol_types::SolType,
};
use eyre::Result;
use subxt::{
	OnlineClient, backend::legacy::LegacyRpcMethods, backend::rpc::RpcClient, utils::H256,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	contracts::socket::Socket_Struct::{Instruction, RequestID, Socket_Message, Task_Params},
	eth::BootstrapState,
	substrate::{CustomConfig, bifrost_runtime},
	utils::sub_display_format,
};

use crate::eth::{ClientMap, EthClient, traits::BootstrapHandler};

const SUB_LOG_TARGET: &str = "socket-onflight-handler";

/// Message sent from SocketOnflightHandler to SocketRelayHandler.
#[derive(Clone, Debug)]
pub struct SocketOnflightMessage {
	/// The decoded socket message.
	pub socket_message: Socket_Message,
	/// The asset index hash.
	pub asset_index_hash: H256,
	/// The sequence ID.
	pub sequence_id: u128,
}

/// Sender type for SocketOnflight messages.
pub type SocketOnflightSender = UnboundedSender<SocketOnflightMessage>;
/// Receiver type for SocketOnflight messages.
pub type SocketOnflightReceiver = UnboundedReceiver<SocketOnflightMessage>;

/// Represents a Standard transfer pending block confirmations.
#[derive(Clone, Debug)]
struct PendingStandardTransfer {
	/// The decoded socket message.
	socket_message: Socket_Message,
	/// The asset index hash.
	asset_index_hash: H256,
	/// The sequence ID.
	sequence_id: u128,
	/// The Substrate block number when this transfer was received.
	received_at_block: u64,
	/// The destination chain ID (for routing to correct SocketRelayHandler).
	dst_chain_id: ChainId,
}

/// The handler that processes `TransferPolled` events from Bifrost Substrate chain.
///
/// This is a single-instance handler that:
/// 1. Subscribes to Bifrost Substrate blocks
/// 2. Processes TransferPolled events from cccp-relay-queue pallet
/// 3. For Fast transfers: immediately sends to destination chain's SocketRelayHandler
/// 4. For Standard transfers: waits for block_confirmations, then sends to SocketRelayHandler
pub struct SocketOnflightHandler<F, P, N: Network>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// The bifrost client (used for block_confirmations config).
	bifrost_client: Arc<EthClient<F, P, N>>,
	/// The entire clients instantiated in the system. <chain_id, Arc<EthClient>>
	system_clients: Arc<ClientMap<F, P, N>>,
	/// The Substrate client for event subscription.
	sub_client: OnlineClient<CustomConfig>,
	/// The legacy RPC methods for querying best block.
	sub_rpc: LegacyRpcMethods<CustomConfig>,
	/// The senders for each chain's SocketRelayHandler. <chain_id, Sender>
	onflight_senders: Arc<BTreeMap<ChainId, SocketOnflightSender>>,
	/// The bootstrap shared data.
	bootstrap_shared_data: Arc<BootstrapSharedData>,
	/// Pending Standard transfers waiting for block confirmations.
	pending_standard_transfers: Arc<RwLock<Vec<PendingStandardTransfer>>>,
}

impl<F, P, N: Network> SocketOnflightHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	/// Instantiates a new `SocketOnflightHandler` instance.
	pub async fn new(
		bifrost_client: Arc<EthClient<F, P, N>>,
		system_clients: Arc<ClientMap<F, P, N>>,
		sub_client: OnlineClient<CustomConfig>,
		sub_rpc_url: String,
		onflight_senders: Arc<BTreeMap<ChainId, SocketOnflightSender>>,
		bootstrap_shared_data: Arc<BootstrapSharedData>,
	) -> Result<Self> {
		let rpc_client = RpcClient::from_url(&sub_rpc_url).await?;
		let sub_rpc = LegacyRpcMethods::<CustomConfig>::new(rpc_client);

		Ok(Self {
			bifrost_client,
			system_clients,
			sub_client,
			sub_rpc,
			onflight_senders,
			bootstrap_shared_data,
			pending_standard_transfers: Arc::new(RwLock::new(Vec::new())),
		})
	}

	/// Run the handler main loop.
	pub async fn run(&mut self) -> Result<()> {
		// Always run bootstrap - recovery should happen regardless of bootstrap setting
		// This avoids race condition with SocketRelayHandler which shares the same chain_id
		self.bootstrap().await?;

		// Note: No wait_for_all_chains_bootstrapped() here
		// Messages sent to SocketRelayHandler are buffered in the channel
		// and processed once SocketRelayHandler finishes its bootstrap and starts main loop.
		// This allows us to catch TransferPolled events that occur during other handlers' bootstrap.

		// Subscribe to Substrate blocks for TransferPolled events
		let mut sub_block_stream = self.sub_client.blocks().subscribe_best().await?;

		log::info!(
			target: &self.bifrost_client.get_chain_name(),
			"-[{}] 🚀 Started listening for OnFlight transfers",
			sub_display_format(SUB_LOG_TARGET),
		);

		while let Some(Ok(block)) = sub_block_stream.next().await {
			if let Err(e) = self.process_substrate_block(block).await {
				br_primitives::log_and_capture!(
					error,
					&self.bifrost_client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bifrost_client.address().await,
					"❗️ Error processing Substrate block: {:?}",
					e
				);
			}
		}

		Ok(())
	}

	/// Process a Substrate block for TransferPolled events.
	async fn process_substrate_block(
		&self,
		block: subxt::blocks::Block<CustomConfig, OnlineClient<CustomConfig>>,
	) -> Result<()> {
		use bifrost_runtime::runtime_types::pallet_cccp_relay_queue::{
			TransferOption, TransferStatus,
		};

		let current_block_number = block.number() as u64;

		// First, process any pending Standard transfers that have been confirmed
		self.process_pending_standard_transfers(current_block_number).await;

		let events = block.events().await?;

		for event in events.iter() {
			let event = match event {
				Ok(e) => e,
				Err(_) => continue,
			};

			// Check if this is a TransferPolled event from cccp_relay_queue pallet
			if event.pallet_name() == "CccpRelayQueue" && event.variant_name() == "TransferPolled" {
				// Decode the event
				if let Ok(transfer_polled) =
					event
						.as_root_event::<bifrost_runtime::cccp_relay_queue::events::TransferPolled>(
						) {
					// Only process if status is OnFlight (quorum reached)
					let is_on_flight = matches!(transfer_polled.status, TransferStatus::OnFlight);
					if !is_on_flight {
						log::debug!(
							target: &self.bifrost_client.get_chain_name(),
							"-[{}] TransferPolled with status {:?}, waiting for quorum (seq={:?})",
							sub_display_format(SUB_LOG_TARGET),
							transfer_polled.status,
							transfer_polled.sequence_id,
						);
						continue;
					}

					let asset_index_hash = H256(transfer_polled.asset_index_hash.0);
					let sequence_id = {
						let u256 = transfer_polled.sequence_id;
						u256.0[0] as u128 | ((u256.0[1] as u128) << 64)
					};

					// Query the socket message from storage
					let socket_message_bytes =
						match self.query_on_flight_transfer(asset_index_hash, sequence_id).await? {
							Some(bytes) => bytes,
							None => {
								log::warn!(
									target: &self.bifrost_client.get_chain_name(),
									"-[{}] Transfer not found in OnFlightTransfers: asset={}, seq={}",
									sub_display_format(SUB_LOG_TARGET),
									asset_index_hash,
									sequence_id,
								);
								continue;
							},
						};

					// Decode the socket message
					let socket_message = match self.decode_socket_message(&socket_message_bytes) {
						Ok(msg) => msg,
						Err(e) => {
							br_primitives::log_and_capture!(
								error,
								&self.bifrost_client.get_chain_name(),
								SUB_LOG_TARGET,
								self.bifrost_client.address().await,
								"❗️ Failed to decode socket message: {:?}",
								e
							);
							continue;
						},
					};

					// Determine the destination chain
					let dst_chain_id =
						Into::<u32>::into(socket_message.ins_code.ChainIndex) as ChainId;

					// Check TransferOption: Fast or Standard
					let is_fast = matches!(transfer_polled.option, TransferOption::Fast);

					if is_fast {
						// Fast transfer: send immediately to destination chain handler
						log::info!(
							target: &self.bifrost_client.get_chain_name(),
							"-[{}] ⚡ OnFlight (Fast) reached quorum: asset={}, seq={}, dst_chain={}",
							sub_display_format(SUB_LOG_TARGET),
							asset_index_hash,
							sequence_id,
							dst_chain_id,
						);

						self.send_to_socket_relay_handler(
							dst_chain_id,
							socket_message,
							asset_index_hash,
							sequence_id,
						)
						.await;
					} else {
						// Standard transfer: add to pending and wait for block confirmations
						let block_confirmations = self.get_block_confirmations(dst_chain_id);

						log::info!(
							target: &self.bifrost_client.get_chain_name(),
							"-[{}] 📋 OnFlight (Standard) reached quorum: asset={}, seq={}, dst_chain={}, waiting for {} block confirmations",
							sub_display_format(SUB_LOG_TARGET),
							asset_index_hash,
							sequence_id,
							dst_chain_id,
							block_confirmations,
						);

						let pending_transfer = PendingStandardTransfer {
							socket_message,
							asset_index_hash,
							sequence_id,
							received_at_block: current_block_number,
							dst_chain_id,
						};

						self.pending_standard_transfers.write().await.push(pending_transfer);
					}
				}
			}
		}

		Ok(())
	}

	/// Process pending Standard transfers that have received enough block confirmations.
	async fn process_pending_standard_transfers(&self, current_block_number: u64) {
		// Collect transfers that are ready to be processed
		let ready_transfers: Vec<PendingStandardTransfer> = {
			let pending = self.pending_standard_transfers.read().await;
			pending
				.iter()
				.filter(|t| {
					let block_confirmations = self.get_block_confirmations(t.dst_chain_id);
					current_block_number.saturating_sub(t.received_at_block) >= block_confirmations
				})
				.cloned()
				.collect()
		};

		if ready_transfers.is_empty() {
			return;
		}

		// Remove ready transfers from pending list
		{
			let mut pending = self.pending_standard_transfers.write().await;
			pending.retain(|t| {
				let block_confirmations = self.get_block_confirmations(t.dst_chain_id);
				current_block_number.saturating_sub(t.received_at_block) < block_confirmations
			});
		}

		// Process each ready transfer
		for transfer in ready_transfers {
			log::info!(
				target: &self.bifrost_client.get_chain_name(),
				"-[{}] ✅ Standard transfer confirmed after {} blocks: asset={}, seq={}, dst_chain={}",
				sub_display_format(SUB_LOG_TARGET),
				current_block_number.saturating_sub(transfer.received_at_block),
				transfer.asset_index_hash,
				transfer.sequence_id,
				transfer.dst_chain_id,
			);

			self.send_to_socket_relay_handler(
				transfer.dst_chain_id,
				transfer.socket_message,
				transfer.asset_index_hash,
				transfer.sequence_id,
			)
			.await;
		}
	}

	/// Get block_confirmations for a specific chain.
	fn get_block_confirmations(&self, chain_id: ChainId) -> u64 {
		self.system_clients
			.get(&chain_id)
			.map(|client| client.metadata.block_confirmations)
			.unwrap_or(self.bifrost_client.metadata.block_confirmations)
	}

	/// Send the processed OnFlight message to the appropriate SocketRelayHandler.
	async fn send_to_socket_relay_handler(
		&self,
		dst_chain_id: ChainId,
		socket_message: Socket_Message,
		asset_index_hash: H256,
		sequence_id: u128,
	) {
		if let Some(sender) = self.onflight_senders.get(&dst_chain_id) {
			let msg = SocketOnflightMessage { socket_message, asset_index_hash, sequence_id };
			if let Err(e) = sender.send(msg) {
				br_primitives::log_and_capture!(
					error,
					&self.bifrost_client.get_chain_name(),
					SUB_LOG_TARGET,
					self.bifrost_client.address().await,
					"❗️ Failed to send OnFlight message to chain {}: {:?}",
					dst_chain_id,
					e
				);
			}
		} else {
			log::warn!(
				target: &self.bifrost_client.get_chain_name(),
				"-[{}] No SocketRelayHandler found for chain {}, skipping OnFlight message",
				sub_display_format(SUB_LOG_TARGET),
				dst_chain_id,
			);
		}
	}

	/// Query OnFlightTransfers storage to get TransferInfo.
	async fn query_on_flight_transfer(
		&self,
		asset_index_hash: H256,
		sequence_id: u128,
	) -> Result<Option<Vec<u8>>> {
		use bifrost_runtime::runtime_types::primitive_types::U256 as RuntimeU256;

		let best_hash = self.sub_rpc.chain_get_block_hash(None).await?.unwrap_or_default();
		let storage = self.sub_client.storage().at(best_hash);

		let transfer_info = storage
			.fetch(&bifrost_runtime::storage().cccp_relay_queue().on_flight_transfers(
				asset_index_hash,
				RuntimeU256([sequence_id as u64, (sequence_id >> 64) as u64, 0, 0]),
			))
			.await?;

		Ok(transfer_info.map(|info| info.socket_message))
	}

	/// Recover pending Standard transfers from OnFlightTransfers storage during startup.
	async fn recover_pending_standard_transfers(&self) -> Result<()> {
		use bifrost_runtime::runtime_types::pallet_cccp_relay_queue::{
			TransferOption, TransferStatus,
		};

		let best_hash = self.sub_rpc.chain_get_block_hash(None).await?.unwrap_or_default();
		let best_block = self.sub_client.blocks().at(best_hash).await?;
		let current_block_number = best_block.number() as u64;

		let storage = self.sub_client.storage().at(best_hash);

		let storage_query =
			bifrost_runtime::storage().cccp_relay_queue().on_flight_transfers_iter();
		let mut iter = storage.iter(storage_query).await?;

		let mut recovered_count = 0u32;

		while let Some(Ok(kv)) = iter.next().await {
			let transfer_info = kv.value;

			// Only process Standard transfers that are OnFlight
			let is_standard = matches!(transfer_info.option, TransferOption::Standard);
			let is_on_flight = matches!(transfer_info.status, TransferStatus::OnFlight);

			if !is_standard || !is_on_flight {
				continue;
			}

			// Decode the socket message
			let socket_message = match self.decode_socket_message(&transfer_info.socket_message) {
				Ok(msg) => msg,
				Err(e) => {
					log::warn!(
						target: &self.bifrost_client.get_chain_name(),
						"-[{}] Failed to decode socket message during recovery: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						e,
					);
					continue;
				},
			};

			let asset_index_hash = H256(socket_message.params.tokenIDX0.0);
			let sequence_id = socket_message.req_id.sequence;
			let dst_chain_id = Into::<u32>::into(socket_message.ins_code.ChainIndex) as ChainId;

			log::info!(
				target: &self.bifrost_client.get_chain_name(),
				"-[{}] 🔄 Recovering Standard transfer: asset={}, seq={}, dst_chain={}, block={}",
				sub_display_format(SUB_LOG_TARGET),
				asset_index_hash,
				sequence_id,
				dst_chain_id,
				current_block_number,
			);

			let pending_transfer = PendingStandardTransfer {
				socket_message,
				asset_index_hash,
				sequence_id,
				received_at_block: current_block_number,
				dst_chain_id,
			};

			self.pending_standard_transfers.write().await.push(pending_transfer);
			recovered_count += 1;
		}

		if recovered_count > 0 {
			log::info!(
				target: &self.bifrost_client.get_chain_name(),
				"-[{}] ✅ Recovered {} pending Standard transfers from storage",
				sub_display_format(SUB_LOG_TARGET),
				recovered_count,
			);
		}

		Ok(())
	}

	/// Decode socket message bytes to Socket_Message struct.
	fn decode_socket_message(&self, bytes: &[u8]) -> Result<Socket_Message> {
		use alloy::sol_types::sol_data;

		type SocketMessageTuple = (
			(sol_data::FixedBytes<4>, sol_data::Uint<64>, sol_data::Uint<128>), // RequestID
			sol_data::Uint<8>,                                                  // status
			(sol_data::FixedBytes<4>, sol_data::FixedBytes<16>),                // Instruction
			(
				sol_data::FixedBytes<32>,
				sol_data::FixedBytes<32>,
				sol_data::Address,
				sol_data::Address,
				sol_data::Uint<256>,
				sol_data::Bytes,
			), // Params
		);

		let decoded = <SocketMessageTuple as SolType>::abi_decode(bytes)
			.map_err(|e| eyre::eyre!("Failed to decode socket message: {}", e))?;

		let (req_id_tuple, status, ins_code_tuple, params_tuple) = decoded;

		let req_id = RequestID {
			ChainIndex: req_id_tuple.0.into(),
			round_id: req_id_tuple.1.try_into().unwrap_or(0),
			sequence: req_id_tuple.2,
		};

		let ins_code =
			Instruction { ChainIndex: ins_code_tuple.0.into(), RBCmethod: ins_code_tuple.1.into() };

		let params = Task_Params {
			tokenIDX0: params_tuple.0,
			tokenIDX1: params_tuple.1,
			refund: params_tuple.2,
			to: params_tuple.3,
			amount: params_tuple.4,
			variants: params_tuple.5.into(),
		};

		Ok(Socket_Message { req_id, status: status.try_into().unwrap_or(0), ins_code, params })
	}
}

#[async_trait::async_trait]
impl<F, P, N: Network> BootstrapHandler for SocketOnflightHandler<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	fn get_chain_id(&self) -> ChainId {
		self.bifrost_client.metadata.id
	}

	fn bootstrap_shared_data(&self) -> Arc<BootstrapSharedData> {
		self.bootstrap_shared_data.clone()
	}

	async fn bootstrap(&self) -> Result<()> {
		let bootstrap_enabled = self
			.bootstrap_shared_data
			.bootstrap_config
			.as_ref()
			.map(|c| c.is_enabled)
			.unwrap_or(false);

		if bootstrap_enabled {
			// Bootstrap enabled: wait for BootstrapSocketRelay phase
			// This ensures RoundUp bootstrap is complete before recovery
			self.wait_for_bootstrap_state(BootstrapState::BootstrapSocketRelay).await?;
		}

		log::info!(
			target: &self.bifrost_client.get_chain_name(),
			"-[{}] 🔄 Recovering pending Standard transfers...",
			sub_display_format(SUB_LOG_TARGET),
		);

		// Always recover pending Standard transfers (regardless of bootstrap setting)
		if let Err(e) = self.recover_pending_standard_transfers().await {
			log::warn!(
				target: &self.bifrost_client.get_chain_name(),
				"-[{}] Failed to recover pending Standard transfers: {:?}",
				sub_display_format(SUB_LOG_TARGET),
				e,
			);
		}

		log::info!(
			target: &self.bifrost_client.get_chain_name(),
			"-[{}] ✅ SocketOnflight bootstrap complete",
			sub_display_format(SUB_LOG_TARGET),
		);

		Ok(())
	}

	async fn get_bootstrap_events(&self) -> Result<Vec<alloy::rpc::types::Log>> {
		Ok(vec![])
	}
}
