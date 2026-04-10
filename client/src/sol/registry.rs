// SPDX-License-Identifier: Apache-2.0
//
// Static asset registry for the cccp-solana outbound path. Maps each
// 32-byte CCCP `AssetIndex` to:
//
//   * the SPL `Mint` pubkey held by the cccp-solana vault, and
//   * the vault PDA's pre-derived associated token account.
//
// Recipient ATAs are derived on the fly from `(recipient_wallet, mint)`
// because we cannot enumerate them at boot.
//
// Operators must keep `sol_provider.assets` in sync with the on-chain
// `init_asset_config` state. If the Bifrost-side Vault gains a 32B
// target slot in the future, the relayer can query this mapping
// off-chain and the static config becomes optional.

use std::collections::HashMap;
use std::str::FromStr;

use solana_sdk::pubkey::Pubkey;

use br_primitives::cli::SolAssetEntry;

use crate::sol::ata::derive_ata;
use crate::sol::codec::AssetIndex;

/// One row in the asset registry. The vault ATA is pre-derived against
/// the cluster's vault PDA so the outbound handler doesn't have to
/// re-derive it for every IX it builds.
#[derive(Debug, Clone)]
pub struct AssetEntry {
	pub asset_index: AssetIndex,
	pub mint: Pubkey,
	pub vault_token_account: Pubkey,
	pub name: Option<String>,
}

/// Per-cluster registry. Built once at boot from `SolProvider.assets`.
#[derive(Debug, Clone, Default)]
pub struct AssetRegistry {
	by_index: HashMap<[u8; 32], AssetEntry>,
}

impl AssetRegistry {
	/// Build a registry from a config-supplied asset list. Pre-derives
	/// every vault ATA against the supplied vault PDA so the outbound
	/// hot path is allocation-free.
	pub fn from_entries(entries: &[SolAssetEntry], vault_pda: &Pubkey) -> eyre::Result<Self> {
		let mut by_index = HashMap::with_capacity(entries.len());
		for entry in entries {
			let index_bytes = parse_asset_index_hex(&entry.index)?;
			let mint = Pubkey::from_str(&entry.mint).map_err(|err| {
				eyre::eyre!(
					"invalid SPL mint pubkey {} for asset {}: {}",
					entry.mint,
					entry.index,
					err
				)
			})?;
			let vault_token_account = derive_ata(vault_pda, &mint);
			by_index.insert(
				index_bytes,
				AssetEntry {
					asset_index: AssetIndex(index_bytes),
					mint,
					vault_token_account,
					name: entry.name.clone(),
				},
			);
		}
		Ok(Self { by_index })
	}

	/// Look up an asset entry by its 32-byte CCCP index.
	pub fn get(&self, index: &[u8; 32]) -> Option<&AssetEntry> {
		self.by_index.get(index)
	}

	pub fn len(&self) -> usize {
		self.by_index.len()
	}

	pub fn is_empty(&self) -> bool {
		self.by_index.is_empty()
	}
}

/// Parse a 32-byte hex string. Accepts both `0x`-prefixed and bare
/// forms. Returns an explicit error if the input is the wrong length.
fn parse_asset_index_hex(s: &str) -> eyre::Result<[u8; 32]> {
	let s = s.strip_prefix("0x").unwrap_or(s);
	if s.len() != 64 {
		eyre::bail!("asset_index must be 32 bytes (= 64 hex chars), got {} chars", s.len());
	}
	let mut out = [0u8; 32];
	hex::decode_to_slice(s, &mut out)
		.map_err(|err| eyre::eyre!("invalid hex in asset_index {s}: {err}"))?;
	Ok(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fake_entries() -> Vec<SolAssetEntry> {
		vec![
			SolAssetEntry {
				index: "0x0100010000000000000000000000000000000000000000000000000000000008".into(),
				mint: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".into(),
				name: Some("usdc".into()),
			},
			SolAssetEntry {
				index: "010001000000000000000000000000000000000000000000000000000000000a".into(),
				mint: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".into(),
				name: None,
			},
		]
	}

	#[test]
	fn parse_hex_with_and_without_prefix() {
		let with = parse_asset_index_hex(
			"0x0100010000000000000000000000000000000000000000000000000000000008",
		)
		.unwrap();
		let without = parse_asset_index_hex(
			"0100010000000000000000000000000000000000000000000000000000000008",
		)
		.unwrap();
		assert_eq!(with, without);
		assert_eq!(with[0], 0x01);
		assert_eq!(with[31], 0x08);
	}

	#[test]
	fn parse_hex_rejects_wrong_length() {
		assert!(parse_asset_index_hex("00").is_err());
		assert!(parse_asset_index_hex("0x00").is_err());
	}

	#[test]
	fn registry_lookup_by_index() {
		let vault_pda = Pubkey::new_unique();
		let registry = AssetRegistry::from_entries(&fake_entries(), &vault_pda).unwrap();
		assert_eq!(registry.len(), 2);

		let mut idx = [0u8; 32];
		idx[0] = 0x01;
		idx[1] = 0x00;
		idx[2] = 0x01;
		idx[31] = 0x08;
		let entry = registry.get(&idx).expect("usdc entry exists");
		assert_eq!(entry.name.as_deref(), Some("usdc"));
		assert_eq!(entry.vault_token_account, derive_ata(&vault_pda, &entry.mint));
	}

	#[test]
	fn invalid_mint_pubkey_rejected() {
		let vault_pda = Pubkey::new_unique();
		let entries = vec![SolAssetEntry {
			index: "0x0000000000000000000000000000000000000000000000000000000000000001".into(),
			mint: "not-a-valid-pubkey".into(),
			name: None,
		}];
		assert!(AssetRegistry::from_entries(&entries, &vault_pda).is_err());
	}
}
