use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

use alloy::{
	network::Network,
	primitives::{B256, ChainId},
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

/// Transfer info result from OnFlightTransfers storage query.
struct TransferInfoResult {
	socket_message: Vec<u8>,
}

/// Represents a Standard transfer pending block confirmations.
#[derive(Clone, Debug)]
struct PendingStandardTransfer {
	/// The decoded socket message.
	socket_message: Socket_Message,
	/// The asset index hash.
	asset_index_hash: H256,
	/// The sequence ID.
	sequence_id: u128,
	/// The source chain ID (for block confirmation checks).
	src_chain_id: ChainId,
	/// The destination chain ID (for routing to correct SocketRelayHandler).
	dst_chain_id: ChainId,
	/// The source transaction hash (for reorg verification).
	src_tx_id: B256,
	/// The source block number when Requested event was emitted (for reorg verification).
	src_block_number: u64,
}

/// Result of source transaction verification.
enum TxVerificationResult {
	/// Transaction is valid (receipt exists).
	Valid,
	/// Transaction receipt not found (reorged out completely).
	NotFound,
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

					// Extract event fields
					let asset_index_hash = H256(transfer_polled.asset_index_hash.0);
					let src_chain_id = transfer_polled.src_chain_id as ChainId;
					let dst_chain_id = transfer_polled.dst_chain_id as ChainId;
					let src_tx_id = B256::from_slice(&transfer_polled.src_tx_id.0);
					let sequence_id = {
						let u256 = transfer_polled.sequence_id;
						u256.0[0] as u128 | ((u256.0[1] as u128) << 64)
					};

					// Query the socket message from storage
					// Storage key: (src_chain_id, src_tx_id)
					let socket_message_bytes = match self
						.query_on_flight_transfer(src_chain_id as u32, H256(src_tx_id.0))
						.await?
					{
						Some(info) => info.socket_message,
						None => {
							log::warn!(
								target: &self.bifrost_client.get_chain_name(),
								"-[{}] Transfer not found in OnFlightTransfers: src_chain={}, src_tx={}",
								sub_display_format(SUB_LOG_TARGET),
								src_chain_id,
								src_tx_id,
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
						// Standard transfer: add to pending and wait for source chain block confirmations
						let block_confirmations = self.get_block_confirmations(src_chain_id);

						// Get current block number from source chain
						let src_block_number =
							match self.get_src_chain_block_number(src_chain_id).await {
								Ok(block) => block,
								Err(e) => {
									log::warn!(
										target: &self.bifrost_client.get_chain_name(),
										"-[{}] Failed to get source chain block number: {:?}",
										sub_display_format(SUB_LOG_TARGET),
										e,
									);
									continue;
								},
							};

						log::info!(
							target: &self.bifrost_client.get_chain_name(),
							"-[{}] 📋 OnFlight (Standard) reached quorum: asset={}, seq={}, src_chain={}, dst_chain={}, src_tx={}, src_block={}, waiting for {} block confirmations",
							sub_display_format(SUB_LOG_TARGET),
							asset_index_hash,
							sequence_id,
							src_chain_id,
							dst_chain_id,
							src_tx_id,
							src_block_number,
							block_confirmations,
						);

						let pending_transfer = PendingStandardTransfer {
							socket_message,
							asset_index_hash,
							sequence_id,
							src_chain_id,
							dst_chain_id,
							src_tx_id,
							src_block_number,
						};

						self.pending_standard_transfers.write().await.push(pending_transfer);
					}
				}
			}
		}

		Ok(())
	}

	/// Process pending Standard transfers that have received enough block confirmations on the source chain.
	async fn process_pending_standard_transfers(&self, _current_block_number: u64) {
		// Get all pending transfers
		let pending_transfers: Vec<PendingStandardTransfer> =
			{ self.pending_standard_transfers.read().await.clone() };

		if pending_transfers.is_empty() {
			return;
		}

		let mut transfers_to_remove: Vec<usize> = Vec::new();
		let mut transfers_to_process: Vec<PendingStandardTransfer> = Vec::new();

		// Track transfers that need block number update (reorg detected but tx still valid)
		let mut transfers_to_update: Vec<(usize, u64)> = Vec::new();

		// Check each pending transfer
		for (idx, transfer) in pending_transfers.iter().enumerate() {
			let block_confirmations = self.get_block_confirmations(transfer.src_chain_id);

			// Get current block number from source chain
			let current_src_block =
				match self.get_src_chain_block_number(transfer.src_chain_id).await {
					Ok(block) => block,
					Err(e) => {
						log::warn!(
							target: &self.bifrost_client.get_chain_name(),
							"-[{}] Failed to get source chain {} block number: {:?}",
							sub_display_format(SUB_LOG_TARGET),
							transfer.src_chain_id,
							e,
						);
						continue;
					},
				};

			// Check if enough blocks have passed on the source chain
			if current_src_block.saturating_sub(transfer.src_block_number) >= block_confirmations {
				// Verify the source transaction is still valid
				match self.verify_src_transaction(transfer.src_chain_id, transfer.src_tx_id).await {
					Ok(TxVerificationResult::Valid) => {
						// Get the actual block number of the transaction to check for reorg
						match self
							.get_tx_block_number(transfer.src_chain_id, transfer.src_tx_id)
							.await
						{
							Ok(Some(tx_block)) if tx_block == transfer.src_block_number => {
								// Block number matches, no reorg, process the transfer
								transfers_to_process.push(transfer.clone());
								transfers_to_remove.push(idx);
							},
							Ok(Some(tx_block)) => {
								// Block number changed due to reorg, update and wait again
								log::info!(
									target: &self.bifrost_client.get_chain_name(),
									"-[{}] 🔄 Reorg detected for tx {}: block {} -> {}, waiting for confirmations again",
									sub_display_format(SUB_LOG_TARGET),
									transfer.src_tx_id,
									transfer.src_block_number,
									tx_block,
								);
								transfers_to_update.push((idx, tx_block));
							},
							Ok(None) => {
								// Transaction not found (shouldn't happen since receipt exists)
								// Be conservative and keep waiting
								log::warn!(
									target: &self.bifrost_client.get_chain_name(),
									"-[{}] Transaction {} has receipt but not found in chain, keeping in pending",
									sub_display_format(SUB_LOG_TARGET),
									transfer.src_tx_id,
								);
							},
							Err(e) => {
								log::warn!(
									target: &self.bifrost_client.get_chain_name(),
									"-[{}] Failed to get tx block number: {:?}",
									sub_display_format(SUB_LOG_TARGET),
									e,
								);
								// Keep in pending list to retry later
							},
						}
					},
					Ok(TxVerificationResult::NotFound) => {
						// Transaction was reorged out completely, remove from pending
						log::warn!(
							target: &self.bifrost_client.get_chain_name(),
							"-[{}] ⚠️ Standard transfer invalidated (reorged out): asset={}, seq={}, src_tx={}",
							sub_display_format(SUB_LOG_TARGET),
							transfer.asset_index_hash,
							transfer.sequence_id,
							transfer.src_tx_id,
						);
						transfers_to_remove.push(idx);
					},
					Err(e) => {
						log::warn!(
							target: &self.bifrost_client.get_chain_name(),
							"-[{}] Failed to verify source transaction: {:?}",
							sub_display_format(SUB_LOG_TARGET),
							e,
						);
						// Keep in pending list to retry later
					},
				}
			}
		}

		// Update block numbers for reorged transfers
		if !transfers_to_update.is_empty() {
			let mut pending = self.pending_standard_transfers.write().await;
			for (idx, new_block) in transfers_to_update {
				if idx < pending.len() {
					pending[idx].src_block_number = new_block;
				}
			}
		}

		// Remove processed/invalidated transfers from pending list
		if !transfers_to_remove.is_empty() {
			let mut pending = self.pending_standard_transfers.write().await;
			// Remove in reverse order to maintain indices
			for idx in transfers_to_remove.into_iter().rev() {
				if idx < pending.len() {
					pending.remove(idx);
				}
			}
		}

		// Process confirmed and valid transfers
		for transfer in transfers_to_process {
			log::info!(
				target: &self.bifrost_client.get_chain_name(),
				"-[{}] ✅ Standard transfer confirmed and verified: asset={}, seq={}, src_chain={}, dst_chain={}, src_tx={}",
				sub_display_format(SUB_LOG_TARGET),
				transfer.asset_index_hash,
				transfer.sequence_id,
				transfer.src_chain_id,
				transfer.dst_chain_id,
				transfer.src_tx_id,
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
	/// Storage key: (src_chain_id, src_tx_id)
	async fn query_on_flight_transfer(
		&self,
		src_chain_id: u32,
		src_tx_id: H256,
	) -> Result<Option<TransferInfoResult>> {
		let best_hash = self.sub_rpc.chain_get_block_hash(None).await?.unwrap_or_default();
		let storage = self.sub_client.storage().at(best_hash);

		let transfer_info = storage
			.fetch(
				&bifrost_runtime::storage()
					.cccp_relay_queue()
					.on_flight_transfers(src_chain_id, src_tx_id),
			)
			.await?;

		Ok(transfer_info.map(|info| TransferInfoResult { socket_message: info.socket_message }))
	}

	/// Query the current block number from a source chain.
	async fn get_src_chain_block_number(&self, src_chain_id: ChainId) -> Result<u64> {
		let client = self
			.system_clients
			.get(&src_chain_id)
			.ok_or_else(|| eyre::eyre!("Source chain client not found: {}", src_chain_id))?;
		Ok(client.get_block_number().await?)
	}

	/// Verify that the source transaction is still valid on the source chain.
	/// Returns Valid if receipt exists, NotFound if transaction was reorged out.
	async fn verify_src_transaction(
		&self,
		src_chain_id: ChainId,
		src_tx_id: B256,
	) -> Result<TxVerificationResult> {
		let client = self
			.system_clients
			.get(&src_chain_id)
			.ok_or_else(|| eyre::eyre!("Source chain client not found: {}", src_chain_id))?;

		// Query the transaction receipt to verify it still exists
		// If the receipt exists, the transaction is mined and valid
		// If the transaction was reorged out, the receipt won't be found
		match client.get_transaction_receipt(src_tx_id).await? {
			Some(_receipt) => Ok(TxVerificationResult::Valid),
			None => {
				// Transaction not found - likely reorged out
				log::warn!(
					target: &self.bifrost_client.get_chain_name(),
					"-[{}] Transaction {} on chain {} not found (possibly reorged out)",
					sub_display_format(SUB_LOG_TARGET),
					src_tx_id,
					src_chain_id,
				);
				Ok(TxVerificationResult::NotFound)
			},
		}
	}

	/// Get the block number of a transaction from the source chain.
	/// Returns None if transaction is not found.
	async fn get_tx_block_number(
		&self,
		src_chain_id: ChainId,
		src_tx_id: B256,
	) -> Result<Option<u64>> {
		use alloy::network::TransactionResponse;

		let client = self
			.system_clients
			.get(&src_chain_id)
			.ok_or_else(|| eyre::eyre!("Source chain client not found: {}", src_chain_id))?;

		// Get the transaction by hash and extract block number
		match client.get_transaction_by_hash(src_tx_id).await? {
			Some(tx) => Ok(tx.block_number()),
			None => Ok(None),
		}
	}

	/// Recover pending Standard transfers from OnFlightTransfers storage during startup.
	async fn recover_pending_standard_transfers(&self) -> Result<()> {
		use bifrost_runtime::runtime_types::pallet_cccp_relay_queue::{
			TransferOption, TransferStatus,
		};

		let best_hash = self.sub_rpc.chain_get_block_hash(None).await?.unwrap_or_default();
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
			let src_chain_id = transfer_info.src_chain_id as ChainId;
			let dst_chain_id = Into::<u32>::into(socket_message.ins_code.ChainIndex) as ChainId;
			let src_tx_id = B256::from_slice(&transfer_info.src_tx_id.0);

			// Get current block number from source chain
			let src_block_number = match self.get_src_chain_block_number(src_chain_id).await {
				Ok(block) => block,
				Err(e) => {
					log::warn!(
						target: &self.bifrost_client.get_chain_name(),
						"-[{}] Failed to get source chain {} block number during recovery: {:?}",
						sub_display_format(SUB_LOG_TARGET),
						src_chain_id,
						e,
					);
					continue;
				},
			};

			log::info!(
				target: &self.bifrost_client.get_chain_name(),
				"-[{}] 🔄 Recovering Standard transfer: asset={}, seq={}, src_chain={}, dst_chain={}, src_tx={}, src_block={}",
				sub_display_format(SUB_LOG_TARGET),
				asset_index_hash,
				sequence_id,
				src_chain_id,
				dst_chain_id,
				src_tx_id,
				src_block_number,
			);

			let pending_transfer = PendingStandardTransfer {
				socket_message,
				asset_index_hash,
				sequence_id,
				src_chain_id,
				dst_chain_id,
				src_tx_id,
				src_block_number,
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
