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

use sha2::{Digest, Sha256};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use br_primitives::{cli::SolProvider, sol::is_sol_chain_index};

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
	expected_upgrade_authority: Option<Pubkey>,
	expected_program_data_sha256: Option<[u8; 32]>,
	asset_attestations: Vec<AssetAttestation>,
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
		let expected_upgrade_authority =
			provider.expected_upgrade_authority.as_deref().map(|value| {
				Pubkey::from_str(value).expect("invalid SolProvider.expected_upgrade_authority")
			});
		let expected_program_data_sha256 =
			provider.expected_program_data_sha256.as_deref().map(|value| {
				let value = value.strip_prefix("0x").unwrap_or(value);
				let mut out = [0u8; 32];
				hex::decode_to_slice(value, &mut out)
					.expect("invalid SolProvider.expected_program_data_sha256");
				out
			});
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

		Self {
			name: provider.name.clone(),
			chain_id: provider.id,
			url: provider.provider.clone(),
			ws_url: provider.ws_provider.clone(),
			program_id,
			is_relay_target: provider.is_relay_target,
			get_signatures_batch_size: provider.get_signatures_batch_size.unwrap_or(100),
			expected_upgrade_authority,
			expected_program_data_sha256,
			asset_attestations,
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

	/// Boot-time deployment attestation. Verifies the executable loader and
	/// ProgramData linkage, optional upgrade-authority/binary hash pins,
	/// singleton account ownership/layout, and every configured asset mint's
	/// address, decimals, initialization state, and mint authority.
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
		let (upgrade_authority, code_offset) = match program_data.data[12] {
			0 => (None, 13usize),
			1 if program_data.data.len() >= 45 => {
				let mut authority = [0u8; 32];
				authority.copy_from_slice(&program_data.data[13..45]);
				(Some(Pubkey::new_from_array(authority)), 45usize)
			},
			other => {
				eyre::bail!("ProgramData {program_data_key} has invalid authority tag {other}")
			},
		};
		if let Some(expected) = self.expected_upgrade_authority
			&& upgrade_authority != Some(expected)
		{
			eyre::bail!(
				"ProgramData upgrade authority mismatch on {}: expected {expected}, got {:?}",
				self.name,
				upgrade_authority,
			);
		}
		let binary_hash: [u8; 32] = Sha256::digest(&program_data.data[code_offset..]).into();
		if let Some(expected) = self.expected_program_data_sha256
			&& binary_hash != expected
		{
			eyre::bail!(
				"ProgramData bytecode hash mismatch on {}: expected {}, got {}",
				self.name,
				hex::encode(expected),
				hex::encode(binary_hash),
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
			"cccp-solana attested: program={} program_data={} sha256={} upgrade_authority={:?} assets={}",
			self.program_id,
			program_data_key,
			hex::encode(binary_hash),
			upgrade_authority,
			self.asset_attestations.len(),
		);
		Ok(slot)
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
}
