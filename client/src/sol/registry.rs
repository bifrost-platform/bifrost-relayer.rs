// SPDX-License-Identifier: Apache-2.0
//
// Finalized on-chain asset registry for the cccp-solana outbound path.
//
// Every AssetConfig has a self-describing AssetDirectoryEntry companion
// account. The relayer enumerates those accounts at boot, validates their
// PDA linkage and the referenced AssetConfig/mint accounts, and builds this
// in-memory routing cache. AssetDirectoryUpdated logs invalidate the cache;
// the event payload itself is never trusted as metadata.

use std::collections::HashMap;

use solana_sdk::pubkey::Pubkey;

use crate::sol::ata::derive_ata;
use crate::sol::codec::AssetIndex;

/// One row in the asset registry. The vault ATA is pre-derived for SPL
/// settlement and ignored when the kind is `NativeCoin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
	pub asset_index: AssetIndex,
	pub mint: Pubkey,
	pub vault_token_account: Pubkey,
	pub decimals: u8,
	pub kind: AssetKind,
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

/// Per-cluster registry reconstructed from finalized directory state.
#[derive(Debug, Clone, Default)]
pub struct AssetRegistry {
	by_index: HashMap<[u8; 32], AssetEntry>,
	native_coin_index: Option<AssetIndex>,
}

impl AssetRegistry {
	pub fn new(
		entries: impl IntoIterator<Item = AssetEntry>,
		native_coin_index: [u8; 32],
	) -> eyre::Result<Self> {
		let mut by_index = HashMap::new();
		for entry in entries {
			let index = entry.asset_index.0;
			if by_index.insert(index, entry).is_some() {
				eyre::bail!("duplicate on-chain asset directory index 0x{}", hex::encode(index));
			}
		}

		let native = by_index.get(&native_coin_index).ok_or_else(|| {
			eyre::eyre!(
				"vault native coin index 0x{} is missing from the on-chain asset directory",
				hex::encode(native_coin_index)
			)
		})?;
		if native.kind != AssetKind::NativeCoin {
			eyre::bail!("vault native coin index is not registered as NativeCoin");
		}

		Ok(Self { by_index, native_coin_index: Some(AssetIndex(native_coin_index)) })
	}

	/// Construct one fully attested entry from finalized AssetConfig state.
	pub fn entry(
		asset_index: [u8; 32],
		mint: Pubkey,
		decimals: u8,
		kind: u8,
		vault_pda: &Pubkey,
	) -> eyre::Result<AssetEntry> {
		Ok(AssetEntry {
			asset_index: AssetIndex(asset_index),
			mint,
			vault_token_account: derive_ata(vault_pda, &mint),
			decimals,
			kind: AssetKind::try_from(kind)?,
		})
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

#[cfg(test)]
mod tests {
	use super::*;

	fn entry(index: [u8; 32], kind: u8, vault: &Pubkey) -> AssetEntry {
		AssetRegistry::entry(index, Pubkey::new_unique(), 9, kind, vault).unwrap()
	}

	#[test]
	fn registry_requires_directory_native_entry() {
		let vault = Pubkey::new_unique();
		let native = [1u8; 32];
		let token = [2u8; 32];
		let registry =
			AssetRegistry::new([entry(native, 1, &vault), entry(token, 2, &vault)], native)
				.unwrap();

		assert_eq!(registry.len(), 2);
		assert_eq!(registry.get(&native).unwrap().kind, AssetKind::NativeCoin);
		assert_eq!(registry.native_coin_index().unwrap(), AssetIndex(native));

		assert!(AssetRegistry::new([entry(token, 2, &vault)], native).is_err());
		assert!(AssetRegistry::new([entry(native, 2, &vault)], native).is_err());
	}

	#[test]
	fn registry_rejects_duplicate_directory_index() {
		let vault = Pubkey::new_unique();
		let native = [1u8; 32];
		assert!(
			AssetRegistry::new([entry(native, 1, &vault), entry(native, 1, &vault)], native)
				.is_err()
		);
	}

	#[test]
	fn entry_derives_spl_vault_ata() {
		let vault = Pubkey::new_unique();
		let mint = Pubkey::new_unique();
		let entry = AssetRegistry::entry([3u8; 32], mint, 6, 2, &vault).unwrap();
		assert_eq!(entry.vault_token_account, derive_ata(&vault, &mint));
		assert_eq!(entry.decimals, 6);
		assert_eq!(entry.kind, AssetKind::SplToken);
	}
}
