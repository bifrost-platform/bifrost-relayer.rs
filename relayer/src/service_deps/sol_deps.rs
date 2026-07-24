// SPDX-License-Identifier: Apache-2.0
//
// Solana dependency container — mirrors `BtcDeps` for the cccp-solana
// integration. One `SolDeps` is built from the relayer's required single
// `SolProvider`.
//
// `SolDeps` is generic in `<F, P, N>` so the outbound handler can hold the
// same Bifrost `EthClient` shape every other relayer subsystem uses;
// `SolOutboundHandler` owns both the BFC → Solana submission path and the
// Solana → BFC commit mirror.

use std::{path::PathBuf, sync::Arc};

use sc_service::SpawnTaskHandle;

use alloy::{
	network::Network as AlloyNetwork,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};

use br_client::{
	eth::EthClient,
	sol::{
		client::SolClient,
		handlers::{
			outbound::{SolOutboundHandler, SolOutboundSender},
			queue_poller::SolSocketQueuePoller,
		},
		registry::AssetRegistry,
		slot_manager::SlotManager,
	},
};
use br_primitives::{
	cli::SolProvider,
	substrate::{CustomConfig, create_rpc_client},
	tx::XtRequestSender,
};
use subxt::{OnlineClient, rpcs::LegacyRpcMethods};

/// Per-cluster Solana dependency bundle.
pub struct SolDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	pub client: SolClient,
	/// Round-derived number of finalized slots to replay before the live
	/// loop starts.
	pub bootstrap_replay_slots: u64,
	pub slot_manager: SlotManager,
	/// Merged outbound worker: BFC → Solana IX submission + Solana → BFC
	/// commit mirror in a single `tokio::select!` loop.
	pub outbound: SolOutboundHandler<F, P, N>,
	/// Send half of the outbound submission queue. The Bifrost socket
	/// relay handler pushes `SolOutboundJob`s here when it has a message
	/// destined for this cluster.
	pub outbound_sender: SolOutboundSender,
	/// Mirrors CCCP socket state transitions (Requested / Committed /
	/// Rollbacked) into the Bifrost `cccp-relay-queue` pallet via
	/// `on_flight_poll` / `finalize_poll` extrinsics. Equivalent of the
	/// EVM `SocketQueuePoller` for the Solana track.
	pub queue_poller: SolSocketQueuePoller<F, P, N>,
}

impl<F, P, N: AlloyNetwork> SolDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	pub fn new(
		provider: &SolProvider,
		client: SolClient,
		registry: AssetRegistry,
		bootstrap_replay_slots: u64,
		bfc_client: Arc<EthClient<F, P, N>>,
		xt_request_sender: Arc<XtRequestSender>,
		sub_client: OnlineClient<CustomConfig>,
		sub_rpc: LegacyRpcMethods<subxt::config::RpcConfigFor<CustomConfig>>,
		handle: SpawnTaskHandle,
		debug_mode: bool,
	) -> eyre::Result<Self> {
		let slot_manager =
			SlotManager::new(client.clone(), provider.call_interval, provider.ws_provider.clone());
		let fee_payer_path = provider.fee_payer_keypair_path.as_ref().map(PathBuf::from);
		// Give the commit mirror its own lossless consumer queue so it cannot
		// be starved by the queue poller's BFC storage queries.
		let (outbound, outbound_sender) = SolOutboundHandler::new(
			client.clone(),
			fee_payer_path,
			registry,
			provider.base_priority_fee,
			provider.max_priority_fee,
			provider.confirmation_timeout_secs,
			provider.max_send_retries,
			bfc_client.clone(),
			handle,
			debug_mode,
			slot_manager.subscribe(),
		)?;

		// The queue poller gets a separate acknowledged delivery stream;
		// cursor advancement waits for both consumers.
		let queue_poller = SolSocketQueuePoller::new(
			client.clone(),
			bfc_client,
			sub_client,
			sub_rpc,
			xt_request_sender,
			slot_manager.subscribe(),
		);

		Ok(Self {
			client,
			bootstrap_replay_slots,
			slot_manager,
			outbound,
			outbound_sender,
			queue_poller,
		})
	}
}

/// Build the single required `SolDeps` bundle from `config.sol_provider`.
///
/// **Boot-time health check**: the configured cluster is probed via
/// `SolClient::health_check` before its `SolDeps` is returned. A
/// misconfigured RPC endpoint, an unreachable program ID, or a
/// `cccp-solana` deployment that hasn't landed yet causes the relayer
/// to refuse to start — exactly the same fail-fast policy the EVM and
/// BTC tracks use.
pub async fn build_sol_deps<F, P, N: AlloyNetwork>(
	provider: &SolProvider,
	bfc_client: Arc<EthClient<F, P, N>>,
	xt_request_sender: Arc<XtRequestSender>,
	sub_client: OnlineClient<CustomConfig>,
	sub_rpc_url: &str,
	handle: SpawnTaskHandle,
	debug_mode: bool,
	bootstrap_round_offset: u64,
) -> eyre::Result<SolDeps<F, P, N>>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	let sub_rpc = {
		let rpc_client = create_rpc_client(sub_rpc_url)
			.await
			.map_err(|e| eyre::eyre!("create substrate RPC client for queue poller: {e}"))?;
		LegacyRpcMethods::<subxt::config::RpcConfigFor<CustomConfig>>::new(rpc_client)
	};

	let bootstrap_round_length = bfc_client
		.protocol_contracts
		.authority
		.round_info()
		.call()
		.await
		.map_err(|e| eyre::eyre!("read BFC round info for Solana bootstrap: {e}"))?
		.round_length
		.saturating_to::<u64>();

	let client = SolClient::new(provider)?;
	let report = client.health_check().await?;
	let bootstrap_replay_slots = client
		.get_bootstrap_replay_slots_based_on_block_time(
			bootstrap_round_offset,
			bootstrap_round_length,
		)
		.await?;
	let deps = SolDeps::new(
		provider,
		client,
		report.registry,
		bootstrap_replay_slots,
		bfc_client,
		xt_request_sender,
		sub_client,
		sub_rpc,
		handle,
		debug_mode,
	)?;
	log::info!(
		target: &deps.client.get_chain_name(),
		"[sol-bootstrap] cluster {} healthy at slot {} (program={})",
		deps.client.name,
		report.slot,
		deps.client.program_id,
	);

	Ok(deps)
}
