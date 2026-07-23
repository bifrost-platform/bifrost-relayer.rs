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

use std::{collections::HashMap, sync::Arc};

use sha2::{Digest, Sha256};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use br_primitives::{
	cli::SolProvider,
	constants::config::NATIVE_BLOCK_TIME,
	sol::{is_sol_chain_index, sol_environment},
};

use crate::sol::pda;

/// Number of recent finalized Solana slots used to estimate wall-clock
/// slot time for round-based bootstrap replay.
const SOL_BOOTSTRAP_SLOT_SAMPLE: u64 = 100;

fn bootstrap_slots_from_timing(
	round_offset: u64,
	round_length: u64,
	slot_diff: u64,
	timestamp_diff: u64,
) -> eyre::Result<u64> {
	if round_offset == 0 || round_length == 0 {
		eyre::bail!("bootstrap round offset and round length must be non-zero");
	}
	if slot_diff == 0 || timestamp_diff == 0 {
		eyre::bail!("Solana bootstrap slot-time sample must span non-zero slots and time");
	}

	// Same time window as EthClient::get_bootstrap_offset_height_based_on_block_time:
	// round_offset × BFC round_length × native BFC block time. Convert that
	// wall-clock duration into Solana slots using the sampled slot cadence.
	let replay_seconds = round_offset
		.checked_mul(round_length)
		.and_then(|blocks| blocks.checked_mul(NATIVE_BLOCK_TIME))
		.ok_or_else(|| eyre::eyre!("Solana bootstrap replay duration overflow"))?;
	let scaled_slots = replay_seconds
		.checked_mul(slot_diff)
		.ok_or_else(|| eyre::eyre!("Solana bootstrap slot calculation overflow"))?;

	Ok(scaled_slots.div_ceil(timestamp_diff).max(1))
}

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
///  76.. 80   bfc_chain_index: ChainIndex ( 4)
///  80.. 96   sequence: u128           (16)
///  96..104   request_timeout: i64     ( 8)
/// 104..112   latest_round_id: u64     ( 8)   ← here
/// 112..120   active_rounds_size: u64  ( 8)
/// 120..121   bump: u8                 ( 1)
/// ```
///
/// Anchor serializes primitive integers as little-endian (borsh contract),
/// so the relayer reads the 8 bytes starting at offset 104 and decodes
/// them with `u64::from_le_bytes`.
const SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET: usize = 104;
const SOCKET_CONFIG_THIS_CHAIN_OFFSET: usize = 72;

/// Byte offset of the `active_rounds_size: u64` field inside the Anchor-
/// serialized `SocketConfig` account (immediately after `latest_round_id`,
/// see the layout table above).
const SOCKET_CONFIG_ACTIVE_ROUNDS_SIZE_OFFSET: usize = 112;

/// Fixed-prefix offset of the `relayers` Vec length (`u32`, little-endian)
/// inside an Anchor-serialized `RoundInfo` account:
///
/// ```text
///   0.. 8   Anchor discriminator
///   8..40   relayer_hash: [u8; 32]
///  40..44   relayers Vec length (u32 LE)   ← here
///  44..44+len*20   relayers: [[u8; 20]; len]
///   ..+8    round_id: u64
///   ..+1    bump: u8
///   ..+32   payer: Pubkey                  (appended by the round-rent feature)
/// ```
///
/// `payer` sits AFTER the variable-length `relayers` Vec, so its byte
/// offset depends on the relayer count and must be computed at decode time
/// via [`round_info_payer_offset`].
const ROUND_INFO_RELAYERS_LEN_OFFSET: usize = 40;

/// Compute the byte offset of `RoundInfo.payer` given the decoded
/// `relayers` Vec length: `disc(8) + relayer_hash(32) + vec_len(4) +
/// relayers(len*20) + round_id(8) + bump(1)`.
fn round_info_payer_offset(relayers_len: usize) -> usize {
	8 + 32 + 4 + relayers_len * 20 + 8 + 1
}

/// Decode `RoundInfo.payer` from a raw account data buffer. Returns `None`
/// when the buffer is too short to hold the field — which is exactly the
/// case for rounds created before the round-rent feature shipped (their
/// account was allocated under the old, payer-less layout), so those
/// rounds are correctly treated as having no recorded payer and are never
/// closable.
fn decode_round_info_payer(data: &[u8]) -> Option<Pubkey> {
	if data.len() < ROUND_INFO_RELAYERS_LEN_OFFSET + 4 {
		return None;
	}
	let mut len_bytes = [0u8; 4];
	len_bytes
		.copy_from_slice(&data[ROUND_INFO_RELAYERS_LEN_OFFSET..ROUND_INFO_RELAYERS_LEN_OFFSET + 4]);
	let relayers_len = u32::from_le_bytes(len_bytes) as usize;

	let off = round_info_payer_offset(relayers_len);
	if data.len() < off + 32 {
		return None;
	}
	let mut payer = [0u8; 32];
	payer.copy_from_slice(&data[off..off + 32]);
	Some(Pubkey::new_from_array(payer))
}

#[derive(Clone)]
struct AssetAttestation {
	index: [u8; 32],
	mint: Pubkey,
	expected_decimals: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct SolHealthReport {
	pub slot: u64,
	pub native_coin_index: [u8; 32],
	pub asset_kinds: HashMap<[u8; 32], u8>,
}

#[derive(Clone)]
pub struct SolClient {
	/// ChainId-derived cluster name (e.g. `solana-devnet`). Used in logs and
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
	upgrade_authority: Pubkey,
	asset_attestations: Vec<AssetAttestation>,
	/// Underlying nonblocking RPC client. Bridge ingestion is always
	/// finalized so callers cannot weaken finality through configuration.
	rpc: Arc<RpcClient>,
}

impl SolClient {
	/// Build a client from a config entry plus the immutable deployment
	/// profile selected by its ChainId.
	///
	/// Environments without a pinned deployment fail closed instead of
	/// accepting a placeholder program identity from operator config.
	pub fn new(provider: &SolProvider) -> eyre::Result<Self> {
		let environment = sol_environment(provider.id)
			.ok_or_else(|| eyre::eyre!("unsupported Solana ChainId {}", provider.id))?;
		let deployment = environment.deployment.ok_or_else(|| {
			eyre::eyre!(
				"cccp-solana deployment identity is not configured for ChainId {} ({})",
				provider.id,
				environment.name
			)
		})?;
		let program_id = Pubkey::from_str(deployment.program_id)
			.map_err(|err| eyre::eyre!("invalid compiled cccp-solana program ID: {err}"))?;

		let rpc = Arc::new(RpcClient::new_with_commitment(
			provider.provider.clone(),
			CommitmentConfig::finalized(),
		));
		let upgrade_authority = Pubkey::from_str(deployment.upgrade_authority)
			.map_err(|err| eyre::eyre!("invalid compiled cccp-solana upgrade authority: {err}"))?;
		let asset_attestations = provider
			.assets
			.iter()
			.map(|entry| {
				let index = entry.index.strip_prefix("0x").unwrap_or(&entry.index);
				let mut index_bytes = [0u8; 32];
				hex::decode_to_slice(index, &mut index_bytes)
					.expect("invalid SolProvider.assets[].index");
				AssetAttestation {
					index: index_bytes,
					mint: Pubkey::from_str(&entry.mint).expect("invalid SolProvider.assets[].mint"),
					expected_decimals: entry.decimals,
				}
			})
			.collect();

		Ok(Self {
			name: environment.name.to_owned(),
			chain_id: provider.id,
			url: provider.provider.clone(),
			ws_url: provider.ws_provider.clone(),
			program_id,
			is_relay_target: provider.is_relay_target,
			get_signatures_batch_size: provider.get_signatures_batch_size.unwrap_or(100),
			upgrade_authority,
			asset_attestations,
			rpc,
		})
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

	/// Convert the global BFC bootstrap round horizon into a Solana slot
	/// horizon using recent finalized block times.
	///
	/// This mirrors `EthClient::get_bootstrap_offset_height_based_on_block_time`
	/// but samples Solana's slot cadence. `getBlocks` filters out skipped
	/// slots; using the first/last slot-number difference still includes
	/// those gaps in the wall-clock conversion.
	pub async fn get_bootstrap_replay_slots_based_on_block_time(
		&self,
		round_offset: u64,
		round_length: u64,
	) -> eyre::Result<u64> {
		let tip = self
			.rpc
			.get_slot()
			.await
			.map_err(|e| eyre::eyre!("get_slot for Solana bootstrap timing: {e}"))?;
		let sample_start = tip.saturating_sub(SOL_BOOTSTRAP_SLOT_SAMPLE);
		let blocks = self
			.rpc
			.get_blocks_with_commitment(sample_start, Some(tip), CommitmentConfig::finalized())
			.await
			.map_err(|e| eyre::eyre!("get_blocks for Solana bootstrap timing: {e}"))?;
		let first_slot = *blocks
			.first()
			.ok_or_else(|| eyre::eyre!("Solana bootstrap timing sample returned no blocks"))?;
		let last_slot = *blocks
			.last()
			.ok_or_else(|| eyre::eyre!("Solana bootstrap timing sample returned no blocks"))?;
		if first_slot == last_slot {
			eyre::bail!("Solana bootstrap timing sample requires at least two finalized blocks");
		}

		let (first_timestamp, last_timestamp) = tokio::try_join!(
			self.rpc.get_block_time(first_slot),
			self.rpc.get_block_time(last_slot),
		)
		.map_err(|e| eyre::eyre!("get_block_time for Solana bootstrap timing: {e}"))?;
		let timestamp_diff = last_timestamp.checked_sub(first_timestamp).ok_or_else(|| {
			eyre::eyre!(
				"non-monotonic Solana block times: slot {first_slot}={first_timestamp}, \
				 slot {last_slot}={last_timestamp}"
			)
		})?;
		let timestamp_diff = u64::try_from(timestamp_diff)
			.map_err(|_| eyre::eyre!("negative Solana bootstrap timestamp difference"))?;
		let slot_diff = last_slot - first_slot;
		let replay_slots =
			bootstrap_slots_from_timing(round_offset, round_length, slot_diff, timestamp_diff)?;

		log::info!(
			target: &self.name,
			"[sol-bootstrap] round_offset={} round_length={} sampled_slots={} sampled_seconds={} replay_slots={}",
			round_offset,
			round_length,
			slot_diff,
			timestamp_diff,
			replay_slots,
		);
		Ok(replay_slots)
	}

	/// Boot-time deployment attestation. Verifies the executable loader and
	/// ProgramData linkage and the compiled upgrade-authority pin,
	/// singleton account ownership/layout, and every configured asset mint's
	/// address, decimals, initialization state, and mint authority.
	///
	/// Returns the latest slot on success. Should be called once during
	/// `service::relay()` setup so misconfigurations fail fast at boot
	/// instead of silently producing zero traffic at runtime.
	pub async fn health_check(&self) -> eyre::Result<SolHealthReport> {
		let slot = self
			.rpc
			.get_slot()
			.await
			.map_err(|e| eyre::eyre!("get_slot probe failed for {}: {e}", self.name))?;

		let upgradeable_loader = Pubkey::from_str("BPFLoaderUpgradeab1e11111111111111111111111")
			.expect("canonical upgradeable loader id");
		let token_program = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
			.expect("canonical token program id");
		let program = self.rpc.get_account(&self.program_id).await.map_err(|err| {
			eyre::eyre!(
				"cccp-solana program {} not found on {} (slot={slot}): {err}",
				self.program_id,
				self.name
			)
		})?;
		if !program.executable || program.owner != upgradeable_loader {
			eyre::bail!(
				"program {} on {} is not executable under the upgradeable loader",
				self.program_id,
				self.name,
			);
		}
		if program.data.len() < 36 || program.data[..4] != 2u32.to_le_bytes() {
			eyre::bail!("program {} has invalid upgradeable-loader state", self.program_id);
		}
		let mut program_data_key = [0u8; 32];
		program_data_key.copy_from_slice(&program.data[4..36]);
		let program_data_key = Pubkey::new_from_array(program_data_key);
		let program_data = self.rpc.get_account(&program_data_key).await.map_err(|err| {
			eyre::eyre!("ProgramData {program_data_key} missing on {}: {err}", self.name)
		})?;
		if program_data.owner != upgradeable_loader
			|| program_data.data.len() < 13
			|| program_data.data[..4] != 3u32.to_le_bytes()
		{
			eyre::bail!("ProgramData {program_data_key} has invalid loader state");
		}
		let upgrade_authority = match program_data.data[12] {
			0 => None,
			1 if program_data.data.len() >= 45 => {
				let mut authority = [0u8; 32];
				authority.copy_from_slice(&program_data.data[13..45]);
				Some(Pubkey::new_from_array(authority))
			},
			other => {
				eyre::bail!("ProgramData {program_data_key} has invalid authority tag {other}")
			},
		};
		if upgrade_authority != Some(self.upgrade_authority) {
			eyre::bail!(
				"ProgramData upgrade authority mismatch on {}: expected {}, got {:?}",
				self.name,
				self.upgrade_authority,
				upgrade_authority,
			);
		}

		let (socket_key, _) = pda::socket_config(&self.program_id);
		let (vault_key, _) = pda::vault_config(&self.program_id);
		let socket = self.rpc.get_account(&socket_key).await.map_err(|err| {
			eyre::eyre!("socket_config {socket_key} missing on {}: {err}", self.name)
		})?;
		let vault = self.rpc.get_account(&vault_key).await.map_err(|err| {
			eyre::eyre!("vault_config {vault_key} missing on {}: {err}", self.name)
		})?;
		attest_anchor_account(
			"SocketConfig",
			&socket_key,
			&socket.owner,
			&socket.data,
			&self.program_id,
			121,
		)?;
		attest_anchor_account(
			"VaultConfig",
			&vault_key,
			&vault.owner,
			&vault.data,
			&self.program_id,
			141,
		)?;
		if socket.data[40..72] != vault_key.to_bytes()
			|| vault.data[40..72] != socket_key.to_bytes()
			|| socket.data[8..40] != vault.data[8..40]
		{
			eyre::bail!("socket/vault singleton linkage is inconsistent on {}", self.name);
		}
		let expected_this_chain = u32::try_from(self.chain_id)
			.map_err(|_| eyre::eyre!("Solana chain id {} does not fit ChainIndex", self.chain_id))?
			.to_be_bytes();
		if !is_sol_chain_index(expected_this_chain) {
			eyre::bail!(
				"Solana chain id {} is not a canonical SOL_DEV/SOL_TEST/SOL_MAIN value",
				self.chain_id,
			);
		}
		let actual_this_chain =
			&socket.data[SOCKET_CONFIG_THIS_CHAIN_OFFSET..SOCKET_CONFIG_THIS_CHAIN_OFFSET + 4];
		if actual_this_chain != expected_this_chain {
			eyre::bail!(
				"socket_config this_chain mismatch on {}: config expects 0x{}, on-chain is 0x{}",
				self.name,
				hex::encode(expected_this_chain),
				hex::encode(actual_this_chain),
			);
		}
		if !matches!(&socket.data[76..80], [0, 0, 0x0b, 0xfc] | [0, 0, 0xbf, 0xc0]) {
			eyre::bail!("socket_config has an unsupported BFC ChainIndex on {}", self.name);
		}

		let mut asset_kinds = HashMap::with_capacity(self.asset_attestations.len());
		for spec in &self.asset_attestations {
			let (asset_key, _) = pda::asset_config(&self.program_id, &spec.index);
			let asset = self.rpc.get_account(&asset_key).await.map_err(|err| {
				eyre::eyre!("asset_config {asset_key} missing on {}: {err}", self.name)
			})?;
			attest_anchor_account(
				"AssetConfig",
				&asset_key,
				&asset.owner,
				&asset.data,
				&self.program_id,
				42,
			)?;
			if asset.data[8..40] != spec.mint.to_bytes() {
				eyre::bail!("asset {asset_key} mint does not match configured {}", spec.mint);
			}
			let mint = self.rpc.get_account(&spec.mint).await.map_err(|err| {
				eyre::eyre!("configured mint {} missing on {}: {err}", spec.mint, self.name)
			})?;
			if mint.owner != token_program || mint.data.len() < 46 || mint.data[45] == 0 {
				eyre::bail!("configured mint {} is not an initialized SPL Mint", spec.mint);
			}
			let decimals = mint.data[44];
			if asset.data[40] != decimals || spec.expected_decimals.is_some_and(|d| d != decimals) {
				eyre::bail!(
					"mint decimals mismatch for {} on {}: mint={decimals}, asset={}, expected={:?}",
					spec.mint,
					self.name,
					asset.data[40],
					spec.expected_decimals,
				);
			}
			let asset_kind = asset.data[41];
			if !matches!(asset_kind, 1..=3) {
				eyre::bail!("asset {asset_key} has unsupported kind {asset_kind}");
			}
			asset_kinds.insert(spec.index, asset_kind);
			if asset_kind == 1 && vault.data[104..136] != spec.mint.to_bytes() {
				eyre::bail!("native asset mint does not match vault_config.native_mint");
			}
			if asset_kind == 3 {
				let authority_tag = u32::from_le_bytes(mint.data[0..4].try_into().unwrap());
				if authority_tag != 1 || mint.data[4..36] != vault_key.to_bytes() {
					eyre::bail!("mintable asset {} is not controlled by the vault PDA", spec.mint);
				}
			}
		}

		log::info!(
			target: &self.name,
			"cccp-solana attested: program={} program_data={} upgrade_authority={:?} assets={}",
			self.program_id,
			program_data_key,
			upgrade_authority,
			self.asset_attestations.len(),
		);
		let mut native_coin_index = [0u8; 32];
		native_coin_index.copy_from_slice(&vault.data[72..104]);
		Ok(SolHealthReport { slot, native_coin_index, asset_kinds })
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

	/// Read `(latest_round_id, active_rounds_size)` from `socket_config` in
	/// a single account fetch. Used by the round-rent sweep to compute which
	/// rounds have dropped out of the active window and are safe to close.
	pub async fn round_window(&self) -> eyre::Result<(u64, u64)> {
		let (socket_config_pda, _) = pda::socket_config(&self.program_id);

		let account = self.rpc.get_account(&socket_config_pda).await.map_err(|e| {
			eyre::eyre!("get_account(socket_config={socket_config_pda}) on {}: {e}", self.name)
		})?;

		let need = SOCKET_CONFIG_ACTIVE_ROUNDS_SIZE_OFFSET + 8;
		if account.data.len() < need {
			eyre::bail!(
				"socket_config on {} is too small ({} bytes) — expected at least {} \
				 (program may not be initialized)",
				self.name,
				account.data.len(),
				need,
			);
		}

		let mut latest = [0u8; 8];
		latest.copy_from_slice(
			&account.data
				[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET..SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8],
		);
		let mut active = [0u8; 8];
		active.copy_from_slice(
			&account.data[SOCKET_CONFIG_ACTIVE_ROUNDS_SIZE_OFFSET
				..SOCKET_CONFIG_ACTIVE_ROUNDS_SIZE_OFFSET + 8],
		);
		Ok((u64::from_le_bytes(latest), u64::from_le_bytes(active)))
	}

	/// Batch-fetch the `payer` of each `RoundInfo` PDA for the given round
	/// ids. Returns one entry per input id, in order: `Some(pubkey)` if the
	/// round account exists and carries a recorded payer, `None` if the
	/// account is absent (already closed / never created) or predates the
	/// round-rent layout. Fetches in chunks of 100 to respect the RPC
	/// `getMultipleAccounts` cap.
	pub async fn round_info_payers(&self, round_ids: &[u64]) -> eyre::Result<Vec<Option<Pubkey>>> {
		let mut out = Vec::with_capacity(round_ids.len());
		for chunk in round_ids.chunks(100) {
			let pdas: Vec<Pubkey> =
				chunk.iter().map(|rid| pda::round_info(&self.program_id, *rid).0).collect();
			let accounts = self.rpc.get_multiple_accounts(&pdas).await.map_err(|e| {
				eyre::eyre!(
					"get_multiple_accounts(round_info x{}) on {}: {e}",
					pdas.len(),
					self.name
				)
			})?;
			for account in accounts {
				out.push(account.and_then(|a| decode_round_info_payer(&a.data)));
			}
		}
		Ok(out)
	}
}

fn attest_anchor_account(
	name: &str,
	key: &Pubkey,
	owner: &Pubkey,
	data: &[u8],
	program_id: &Pubkey,
	minimum_size: usize,
) -> eyre::Result<()> {
	if owner != program_id {
		eyre::bail!("{name} {key} is owned by {owner}, expected {program_id}");
	}
	if data.len() < minimum_size {
		eyre::bail!("{name} {key} is too small: {} < {minimum_size}", data.len());
	}
	let expected = {
		let digest = Sha256::digest(format!("account:{name}").as_bytes());
		let mut out = [0u8; 8];
		out.copy_from_slice(&digest[..8]);
		out
	};
	if data[..8] != expected {
		eyre::bail!("{name} {key} has the wrong Anchor discriminator");
	}
	Ok(())
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
		//       + bfc_chain_index(4) + sequence(16) + request_timeout(8)
		//       + latest_round_id(8) + active_rounds_size(8) + bump(1)  = 121 bytes.
		let mut blob = vec![0u8; 121];

		// Anchor account discriminator (first 8 bytes) — arbitrary here.
		blob[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

		// Fill preceding fields with sentinel values so any off-by-one
		// in the offset will produce a clearly wrong `latest_round_id`.
		blob[8..40].copy_from_slice(&[0x11; 32]); // authority
		blob[40..72].copy_from_slice(&[0x22; 32]); // vault
		blob[72..76].copy_from_slice(&[0x33; 4]); // this_chain
		blob[76..80].copy_from_slice(&[0x66; 4]); // bfc_chain_index
		blob[80..96].copy_from_slice(&[0x44; 16]); // sequence
		blob[96..104].copy_from_slice(&[0x55; 8]); // request_timeout

		// latest_round_id = 0x0000_0000_0000_2A = 42 (little-endian).
		let expected: u64 = 42;
		blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET..SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8]
			.copy_from_slice(&expected.to_le_bytes());

		blob[112..120].copy_from_slice(&[0x77; 8]); // active_rounds_size
		blob[120] = 0xff; // bump

		let mut buf = [0u8; 8];
		buf.copy_from_slice(
			&blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET..SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8],
		);
		assert_eq!(u64::from_le_bytes(buf), expected);

		// Smoke-check the surrounding sentinels didn't bleed into the slot.
		assert_eq!(blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET - 1], 0x55);
		assert_eq!(blob[SOCKET_CONFIG_LATEST_ROUND_ID_OFFSET + 8], 0x77);
	}

	/// Build a `RoundInfo` blob the way Anchor would serialize it and assert
	/// `decode_round_info_payer` recovers the payer regardless of the
	/// (variable) relayer count. Mirrors `cccp-solana::state::RoundInfo`.
	#[test]
	fn round_info_payer_decode_matches_layout() {
		for relayers_len in [0usize, 1, 7, 64] {
			let payer_off = round_info_payer_offset(relayers_len);
			// Account allocated at the full max size, payer written at its
			// (variable) offset, tail left as zero padding — exactly how
			// Anchor lays out an `init`-ed account with `space = SIZE`.
			let max_off = round_info_payer_offset(64);
			let mut blob = vec![0u8; max_off + 32];

			blob[0..8].copy_from_slice(&[9u8; 8]); // discriminator
			blob[8..40].copy_from_slice(&[0x11; 32]); // relayer_hash
			blob[ROUND_INFO_RELAYERS_LEN_OFFSET..ROUND_INFO_RELAYERS_LEN_OFFSET + 4]
				.copy_from_slice(&(relayers_len as u32).to_le_bytes());
			// relayers payload + round_id + bump left as zeros — irrelevant.

			let expected = Pubkey::new_unique();
			blob[payer_off..payer_off + 32].copy_from_slice(&expected.to_bytes());

			assert_eq!(decode_round_info_payer(&blob), Some(expected));
		}
	}

	/// A buffer too short to contain the payer (pre-feature layout) decodes
	/// to `None`, so legacy rounds are never treated as closable.
	#[test]
	fn round_info_payer_decode_short_buffer_is_none() {
		// relayers_len = 7 → payer would start at round_info_payer_offset(7),
		// but we truncate the buffer right before it.
		let payer_off = round_info_payer_offset(7);
		let mut blob = vec![0u8; payer_off]; // one byte short of the payer field
		blob[ROUND_INFO_RELAYERS_LEN_OFFSET..ROUND_INFO_RELAYERS_LEN_OFFSET + 4]
			.copy_from_slice(&7u32.to_le_bytes());
		assert_eq!(decode_round_info_payer(&blob), None);
	}

	#[test]
	fn bootstrap_slots_use_bfc_round_window_and_sampled_slot_time() {
		// 3 rounds × 100 BFC blocks × 3s = 900s. A 40s / 100-slot
		// Solana sample is 400ms per slot, so replay 2,250 slots.
		assert_eq!(bootstrap_slots_from_timing(3, 100, 100, 40).unwrap(), 2_250);
	}

	#[test]
	fn bootstrap_slots_round_up_partial_slot() {
		assert_eq!(bootstrap_slots_from_timing(1, 1, 2, 5).unwrap(), 2);
	}

	#[test]
	fn bootstrap_slots_reject_invalid_timing() {
		assert!(bootstrap_slots_from_timing(0, 100, 100, 40).is_err());
		assert!(bootstrap_slots_from_timing(3, 0, 100, 40).is_err());
		assert!(bootstrap_slots_from_timing(3, 100, 0, 40).is_err());
		assert!(bootstrap_slots_from_timing(3, 100, 100, 0).is_err());
	}
}
