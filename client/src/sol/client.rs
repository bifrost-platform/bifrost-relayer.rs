// SPDX-License-Identifier: Apache-2.0
//
// `SolClient` is the relayer's handle to one Solana cluster. It wraps a
// nonblocking `RpcClient` plus the bifrost `EthClient` (so handlers can
// look up things like the cccp config or selected relayer set without
// passing two separate handles around).
//
// At the MVP level the client is intentionally minimal: it only exposes
// the URL, the cccp-solana program ID, and the underlying RPC. Future
// work (slot polling, signature decoding, transaction submission) lives
// in `slot_manager.rs` and `handlers/`.

use std::sync::Arc;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use br_primitives::cli::SolProvider;

#[derive(Clone)]
pub struct SolClient {
	/// Free-form cluster name (e.g. `solana-devnet`). Used in logs and
	/// metrics labels.
	pub name: String,
	/// CCCP `ChainId` for this cluster.
	pub chain_id: u64,
	/// JSON-RPC HTTP endpoint URL.
	pub url: String,
	/// Optional WebSocket endpoint for `slotSubscribe`.
	pub ws_url: Option<String>,
	/// `cccp-solana` program ID parsed from base58.
	pub program_id: Pubkey,
	/// Whether the relayer should send outbound IXs to this cluster.
	pub is_relay_target: bool,
	/// `getSignaturesForAddress` page size.
	pub get_signatures_batch_size: u64,
	/// Underlying nonblocking RPC client. Pre-configured with the
	/// `commitment` level from the provider config.
	rpc: Arc<RpcClient>,
}

impl SolClient {
	/// Build a new client from a config entry. Panics if the program ID
	/// or URL is invalid — those are operator misconfigurations and
	/// should fail loudly at boot, not silently at runtime.
	pub fn new(provider: &SolProvider) -> Self {
		let program_id = Pubkey::from_str(&provider.program_id)
			.expect("invalid SolProvider.program_id (not a base58 pubkey)");

		let commitment = parse_commitment(provider.commitment.as_deref());
		let rpc = Arc::new(RpcClient::new_with_commitment(provider.provider.clone(), commitment));

		Self {
			name: provider.name.clone(),
			chain_id: provider.id,
			url: provider.provider.clone(),
			ws_url: provider.ws_provider.clone(),
			program_id,
			is_relay_target: provider.is_relay_target,
			get_signatures_batch_size: provider.get_signatures_batch_size.unwrap_or(100),
			rpc,
		}
	}

	/// Borrow the underlying `RpcClient`. Handlers use this directly for
	/// `get_slot`, `get_signatures_for_address`, `get_transaction`, etc.
	pub fn rpc(&self) -> Arc<RpcClient> {
		self.rpc.clone()
	}

	/// Just the chain name, used as a `log::target`.
	pub fn get_chain_name(&self) -> String {
		self.name.clone()
	}

	/// Boot-time health check. Verifies that:
	///   1. the configured RPC endpoint is reachable + returns a valid slot,
	///   2. the configured `program_id` actually has an account on chain
	///      (= the cccp-solana program is deployed to this cluster).
	///
	/// Returns the latest slot on success. Should be called once during
	/// `service::relay()` setup so misconfigurations fail fast at boot
	/// instead of silently producing zero traffic at runtime.
	pub async fn health_check(&self) -> eyre::Result<u64> {
		let slot = self
			.rpc
			.get_slot()
			.await
			.map_err(|e| eyre::eyre!("get_slot probe failed for {}: {e}", self.name))?;

		match self.rpc.get_account(&self.program_id).await {
			Ok(_) => Ok(slot),
			Err(err) => Err(eyre::eyre!(
				"cccp-solana program {} not found on {} (slot={slot}): {err}",
				self.program_id,
				self.name
			)),
		}
	}
}

fn parse_commitment(s: Option<&str>) -> CommitmentConfig {
	let level = match s.unwrap_or("finalized") {
		"processed" => CommitmentLevel::Processed,
		"confirmed" => CommitmentLevel::Confirmed,
		// default + canonical: finalized
		_ => CommitmentLevel::Finalized,
	};
	CommitmentConfig { commitment: level }
}
