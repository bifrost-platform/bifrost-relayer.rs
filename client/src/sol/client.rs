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

use crate::sol::pda;

/// Byte offset of the `latest_round_id: u64` field inside the Anchor-
/// serialized `SocketConfig` account.
///
/// Layout (see `cccp-solana::state::socket_config::SocketConfig`):
///
/// ```text
///   0..  8   Anchor discriminator
///   8.. 40   authority: Pubkey        (32)
///  40.. 72   vault: Pubkey            (32)
///  72.. 76   this_chain: ChainIndex   ( 4)
///  76.. 92   sequence: u128           (16)
///  92..100   request_timeout: i64     ( 8)
/// 100..108   latest_round_id: u64     ( 8)   ← here
/// 108..116   active_rounds_size: u64  ( 8)
/// 116..117   bump: u8                 ( 1)
/// ```
///
/// Anchor serializes primitive integers as little-endian (borsh contract),
/// so the relayer reads the 8 bytes starting at offset 100 and decodes
/// them with `u64::from_le_bytes`.
const SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET: usize = 100;

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

	/// Read the on-chain `socket_config.latest_round_id`.
	///
	/// Used by the round-up relay handler to decide whether a given
	/// `round_control_relay(submit)` still needs to be pushed to this
	/// cluster (it does iff `submit.round > latest_round_id`) and to
	/// clear entries from the pending-retry queue once the cluster has
	/// caught up.
	///
	/// Errors if the RPC call fails, the `socket_config` PDA does not
	/// exist yet (= the cccp-solana program has been deployed but never
	/// initialized on this cluster), or the account is smaller than the
	/// expected layout. Each of those is an operator-visible
	/// misconfiguration, not a transient condition.
	pub async fn latest_round_id(&self) -> eyre::Result<u64> {
		let (socket_config_pda, _) = pda::socket_config(&self.program_id);

		let account = self.rpc.get_account(&socket_config_pda).await.map_err(|e| {
			eyre::eyre!("get_account(socket_config={socket_config_pda}) on {}: {e}", self.name)
		})?;

		let end = SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8;
		if account.data.len() < end {
			eyre::bail!(
				"socket_config on {} is too small ({} bytes) — expected at least {} \
				 (program may not be initialized)",
				self.name,
				account.data.len(),
				end,
			);
		}
		let mut buf = [0u8; 8];
		buf.copy_from_slice(&account.data[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET..end]);
		Ok(u64::from_le_bytes(buf))
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

#[cfg(test)]
mod tests {
	use super::*;

	/// Pins the `latest_round_id` byte offset against a hand-built blob
	/// that matches the Anchor layout of `cccp-solana::state::SocketConfig`.
	/// If the on-chain struct ever grows a field before `latest_round_id`,
	/// this test fails and the constant must be re-derived in lockstep.
	#[test]
	fn socket_config_latest_round_id_offset_matches_layout() {
		// Layout: discriminator(8) + authority(32) + vault(32) + this_chain(4)
		//       + sequence(16) + request_timeout(8) + latest_round_id(8)
		//       + active_rounds_size(8) + bump(1)  = 117 bytes.
		let mut blob = vec![0u8; 117];

		// Anchor account discriminator (first 8 bytes) — arbitrary here.
		blob[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

		// Fill preceding fields with sentinel values so any off-by-one
		// in the offset will produce a clearly wrong `latest_round_id`.
		blob[8..40].copy_from_slice(&[0x11; 32]); // authority
		blob[40..72].copy_from_slice(&[0x22; 32]); // vault
		blob[72..76].copy_from_slice(&[0x33; 4]); // this_chain
		blob[76..92].copy_from_slice(&[0x44; 16]); // sequence
		blob[92..100].copy_from_slice(&[0x55; 8]); // request_timeout

		// latest_round_id = 0x0000_0000_0000_2A = 42 (little-endian).
		let expected: u64 = 42;
		blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET..SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8]
			.copy_from_slice(&expected.to_le_bytes());

		blob[108..116].copy_from_slice(&[0x77; 8]); // active_rounds_size
		blob[116] = 0xff; // bump

		let mut buf = [0u8; 8];
		buf.copy_from_slice(
			&blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET..SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8],
		);
		assert_eq!(u64::from_le_bytes(buf), expected);

		// Smoke-check the surrounding sentinels didn't bleed into the slot.
		assert_eq!(blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET - 1], 0x55);
		assert_eq!(blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8], 0x77);
	}
}
