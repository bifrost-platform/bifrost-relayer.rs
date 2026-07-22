// SPDX-License-Identifier: Apache-2.0
//
// Static asset registry for the cccp-solana outbound path. Maps each
// 32-byte CCCP `AssetIndex` to:
//
//   * the SPL `Mint` pubkey held by the cccp-solana vault, and
//   * the vault PDA's pre-derived associated token account.
//
// SPL recipient ATAs are derived on the fly from `(recipient_wallet, mint)`
// because we cannot enumerate them at boot. NativeCoin entries use the direct
// wallet and native-vault PDA; their mint remains attested metadata.
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

/// One row in the asset registry. The vault ATA is pre-derived for SPL
/// settlement and ignored when the attested kind is `NativeCoin`.
#[derive(Debug, Clone)]
pub struct AssetEntry {
	pub asset_index: AssetIndex,
	pub mint: Pubkey,
	pub vault_token_account: Pubkey,
	pub name: Option<String>,
	/// Populated exclusively from the boot-time on-chain AssetConfig
	/// attestation. `None` means startup attestation has not been applied yet.
	pub kind: Option<AssetKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
	NativeCoin,
	SplToken,
	SplTokenMintable,
}

impl TryFrom<u8> for AssetKind {
	type Error = eyre::Error;

	fn try_from(value: u8) -> Result<Self, Self::Error> {
		match value {
			1 => Ok(Self::NativeCoin),
			2 => Ok(Self::SplToken),
			3 => Ok(Self::SplTokenMintable),
			_ => eyre::bail!("unsupported on-chain Solana asset kind {value}"),
		}
	}
}

/// Per-cluster registry. Built once at boot from `SolProvider.assets`.
#[derive(Debug, Clone, Default)]
pub struct AssetRegistry {
	by_index: HashMap<[u8; 32], AssetEntry>,
	native_coin_index: Option<AssetIndex>,
}

impl AssetRegistry {
	/// Build a registry from a config-supplied asset list. Pre-derives
	/// every SPL vault ATA against the supplied vault PDA so the outbound
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
					kind: None,
				},
			);
		}
		Ok(Self { by_index, native_coin_index: None })
	}

	/// Apply the result of the on-chain startup attestation. Native routing is
	/// never inferred from a config name or mint address.
	pub fn apply_attestation(
		&mut self,
		kinds: &HashMap<[u8; 32], u8>,
		native_coin_index: [u8; 32],
	) -> eyre::Result<()> {
		for (index, entry) in &mut self.by_index {
			let raw = kinds.get(index).ok_or_else(|| {
				eyre::eyre!("missing on-chain kind attestation for asset 0x{}", hex::encode(index))
			})?;
			entry.kind = Some(AssetKind::try_from(*raw)?);
		}
		if self.by_index.values().any(|entry| entry.kind == Some(AssetKind::NativeCoin)) {
			let local = self.by_index.get(&native_coin_index).ok_or_else(|| {
				eyre::eyre!(
					"native assets are configured but local native index 0x{} is missing",
					hex::encode(native_coin_index)
				)
			})?;
			if local.kind != Some(AssetKind::NativeCoin) {
				eyre::bail!("vault native coin index is not attested as NativeCoin");
			}
		}
		self.native_coin_index = Some(AssetIndex(native_coin_index));
		Ok(())
	}

	/// Look up an asset entry by its 32-byte CCCP index.
	pub fn get(&self, index: &[u8; 32]) -> Option<&AssetEntry> {
		self.by_index.get(index)
	}

	pub fn native_coin_index(&self) -> eyre::Result<AssetIndex> {
		self.native_coin_index
			.ok_or_else(|| eyre::eyre!("native coin index has not been attested"))
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
				decimals: Some(6),
			},
			SolAssetEntry {
				index: "010001000000000000000000000000000000000000000000000000000000000a".into(),
				mint: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".into(),
				name: None,
				decimals: None,
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
	fn native_kind_comes_from_attestation_and_requires_local_entry() {
		let vault_pda = Pubkey::new_unique();
		let entries = fake_entries();
		let mut registry = AssetRegistry::from_entries(&entries, &vault_pda).unwrap();
		let first = parse_asset_index_hex(&entries[0].index).unwrap();
		let second = parse_asset_index_hex(&entries[1].index).unwrap();
		let kinds = HashMap::from([(first, 1u8), (second, 2u8)]);
		registry.apply_attestation(&kinds, first).unwrap();
		assert_eq!(registry.get(&first).unwrap().kind, Some(AssetKind::NativeCoin));
		assert_eq!(registry.native_coin_index().unwrap(), AssetIndex(first));

		let mut missing_local = AssetRegistry::from_entries(&entries, &vault_pda).unwrap();
		assert!(missing_local.apply_attestation(&kinds, [0xff; 32]).is_err());
	}

	#[test]
	fn invalid_mint_pubkey_rejected() {
		let vault_pda = Pubkey::new_unique();
		let entries = vec![SolAssetEntry {
			index: "0x0000000000000000000000000000000000000000000000000000000000000001".into(),
			mint: "not-a-valid-pubkey".into(),
			name: None,
			decimals: None,
		}];
		assert!(AssetRegistry::from_entries(&entries, &vault_pda).is_err());
	}
}
