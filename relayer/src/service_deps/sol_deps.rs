// SPDX-License-Identifier: Apache-2.0
//
// Solana dependency container — mirrors `BtcDeps` for the cccp-solana
// integration. One `SolDeps` is built per `SolProvider` entry in the
// relayer config; if no providers are configured, no Solana wiring is
// spawned at all.
//
// `SolDeps` is generic in `<F, P, N>` so the inbound + outbound
// handlers can hold the same Bifrost `EthClient` shape every other
// relayer subsystem uses. `SolOutboundHandler` owns both the BFC →
// Solana submission path and the Solana → BFC commit mirror in one
// worker.

use std::path::PathBuf;
use std::sync::Arc;

use alloy::{
	network::Network as AlloyNetwork,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};

use br_client::{
	eth::EthClient,
	sol::{
		client::SolClient,
		handlers::{
			inbound::SolInboundHandler,
			outbound::{SolOutboundHandler, SolOutboundSender},
		},
		pda,
		registry::AssetRegistry,
		slot_manager::SlotManager,
	},
};
use br_primitives::{cli::SolProvider, tx::XtRequestSender};

const DEFAULT_SOL_CONFIRMATION_DEPTH: u64 = 32;

/// Per-cluster Solana dependency bundle.
pub struct SolDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub client: SolClient,
	/// Number of slots to walk backwards on boot, passed to
	/// `SlotManager::bootstrap_catchup` before the live loop starts.
	/// `0` / `None` disables catch-up.
	pub bootstrap_offset_slots: u64,
	pub slot_manager: SlotManager,
	pub inbound: SolInboundHandler<F, P, N>,
	/// Merged outbound worker: BFC → Solana IX submission + Solana → BFC
	/// commit mirror in a single `tokio::select!` loop.
	pub outbound: SolOutboundHandler<F, P, N>,
	/// Send half of the outbound submission queue. The Bifrost socket
	/// relay handler pushes `SolOutboundJob`s here when it has a message
	/// destined for this cluster.
	pub outbound_sender: SolOutboundSender,
}

impl<F, P, N: AlloyNetwork> SolDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub fn new(
		provider: &SolProvider,
		bfc_client: Arc<EthClient<F, P, N>>,
		xt_request_sender: Arc<XtRequestSender>,
	) -> eyre::Result<Self> {
		let client = SolClient::new(provider);

		let confirmation_depth =
			provider.block_confirmations.unwrap_or(DEFAULT_SOL_CONFIRMATION_DEPTH);

		let slot_manager = SlotManager::new(
			client.clone(),
			provider.call_interval,
			confirmation_depth,
			provider.ws_provider.clone(),
		);
		let inbound = SolInboundHandler::new(
			client.clone(),
			bfc_client.clone(),
			xt_request_sender.clone(),
			slot_manager.subscribe(),
		);

		// Build the per-cluster asset registry from the static config.
		// The vault PDA is derived from the cccp-solana program ID and is
		// shared across every asset entry — we pre-derive each vault ATA
		// so the outbound hot path is allocation-free.
		let (vault_pda, _vault_bump) = pda::vault_config(&client.program_id);
		let registry = AssetRegistry::from_entries(&provider.assets, &vault_pda)?;

		let fee_payer_path = PathBuf::from(&provider.fee_payer_keypair_path);
		// Merged outbound worker: gets its own broadcast subscriber so
		// lag on the commit-mirror branch doesn't starve the inbound
		// handler (and vice versa).
		let (outbound, outbound_sender) = SolOutboundHandler::new(
			client.clone(),
			fee_payer_path,
			registry,
			provider.base_priority_fee,
			provider.max_priority_fee,
			provider.confirmation_timeout_secs,
			provider.max_send_retries,
			bfc_client,
			xt_request_sender,
			slot_manager.subscribe(),
		)?;

		let bootstrap_offset_slots = provider.bootstrap_offset_slots.unwrap_or(0);

		Ok(Self {
			client,
			bootstrap_offset_slots,
			slot_manager,
			inbound,
			outbound,
			outbound_sender,
		})
	}
}

/// Build a `SolDeps` bundle for every entry in `config.sol_providers`.
/// Returns an empty vec if no providers are configured. Each entry shares
/// the same Bifrost `EthClient` + `XtRequestSender` for inbound vote
/// submission.
///
/// **Boot-time health check**: every cluster is probed via
/// `SolClient::health_check` before its `SolDeps` is returned. A
/// misconfigured RPC endpoint, an unreachable program ID, or a
/// `cccp-solana` deployment that hasn't landed yet causes the relayer
/// to refuse to start — exactly the same fail-fast policy the EVM and
/// BTC tracks use.
pub async fn build_sol_deps<F, P, N: AlloyNetwork>(
	providers: &[SolProvider],
	bfc_client: Arc<EthClient<F, P, N>>,
	xt_request_sender: Arc<XtRequestSender>,
) -> eyre::Result<Vec<SolDeps<F, P, N>>>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	let mut out = Vec::with_capacity(providers.len());
	for p in providers {
		let deps = SolDeps::new(p, bfc_client.clone(), xt_request_sender.clone())?;
		let slot = deps.client.health_check().await?;
		log::info!(
			target: &deps.client.get_chain_name(),
			"[sol-bootstrap] cluster {} healthy at slot {} (program={})",
			deps.client.name,
			slot,
			deps.client.program_id,
		);

		out.push(deps);
	}
	Ok(out)
}
