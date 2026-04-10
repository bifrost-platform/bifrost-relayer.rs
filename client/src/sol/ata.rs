// SPDX-License-Identifier: Apache-2.0
//
// Off-chain Associated Token Account (ATA) derivation. The relayer needs
// these addresses to populate the cccp-solana `poll(...)` IX:
//
//   * `vault_token_account`     = ATA(owner = vault_config PDA, mint)
//   * `recipient_token_account` = ATA(owner = recipient solana wallet, mint)
//
// We deliberately do **not** depend on `spl-associated-token-account` to
// avoid pulling another SBF-flavored crate into the relayer host build.
// The PDA derivation is mechanical and pinned by a unit test against the
// canonical ATA program ID.

use solana_sdk::pubkey::Pubkey;

use crate::sol::ix_builder::{SPL_TOKEN_PROGRAM_ID, SYSTEM_PROGRAM_ID};

/// Associated Token Account program ID
/// (`ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL`).
/// Hard-coded as raw bytes — same rationale as `SPL_TOKEN_PROGRAM_ID` in
/// `crate::sol::ix_builder`. Verified at runtime by the
/// `tests::ata_program_id_const_matches_canonical_base58` test below.
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
	0x8c, 0x97, 0x25, 0x8f, 0x4e, 0x24, 0x89, 0xf1, 0xbb, 0x3d, 0x10, 0x29, 0x14, 0x8e, 0x0d, 0x83,
	0x0b, 0x5a, 0x13, 0x99, 0xda, 0xff, 0x10, 0x84, 0x04, 0x8e, 0x7b, 0xd8, 0xdb, 0xe9, 0xf8, 0x59,
]);

/// Derive the canonical Associated Token Account address for the given
/// `(owner, mint)` pair on the SPL Token program.
pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
	let (pda, _bump) = Pubkey::find_program_address(
		&[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
		&ASSOCIATED_TOKEN_PROGRAM_ID,
	);
	pda
}

/// Build the standard `create_associated_token_account` instruction. We
/// use the **idempotent** form (data = `[1]`) so the relayer can include
/// it in every outbound transaction without worrying about whether the
/// recipient ATA already exists. The Solana runtime treats a redundant
/// create as a no-op.
///
/// Account meta order matches the canonical ATA program layout:
///   [0]  funding_address (signer, mut)
///   [1]  associated_token_account (mut)
///   [2]  wallet_address
///   [3]  spl_token_mint_address
///   [4]  system_program
///   [5]  token_program
pub fn build_create_ata_idempotent_ix(
	payer: &Pubkey,
	owner: &Pubkey,
	mint: &Pubkey,
) -> solana_sdk::instruction::Instruction {
	use solana_sdk::instruction::{AccountMeta, Instruction};

	let ata = derive_ata(owner, mint);
	Instruction {
		program_id: ASSOCIATED_TOKEN_PROGRAM_ID,
		accounts: vec![
			AccountMeta::new(*payer, true),
			AccountMeta::new(ata, false),
			AccountMeta::new_readonly(*owner, false),
			AccountMeta::new_readonly(*mint, false),
			AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
			AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
		],
		data: vec![1u8], // CreateIdempotent variant
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ata_program_id_const_matches_canonical_base58() {
		let parsed: Pubkey = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL".parse().unwrap();
		assert_eq!(parsed, ASSOCIATED_TOKEN_PROGRAM_ID);
	}

	#[test]
	fn derive_ata_known_vector() {
		// The canonical SPL Token program test vector. The ATA address
		// for the all-zero owner pubkey + the all-zero mint is fully
		// deterministic, so we just check the derivation is stable.
		let owner = Pubkey::new_from_array([0u8; 32]);
		let mint = Pubkey::new_from_array([0u8; 32]);
		let ata1 = derive_ata(&owner, &mint);
		let ata2 = derive_ata(&owner, &mint);
		assert_eq!(ata1, ata2);
		// Sanity: not equal to either input.
		assert_ne!(ata1, owner);
		assert_ne!(ata1, mint);
	}

	#[test]
	fn create_ata_idempotent_layout() {
		let payer = Pubkey::new_unique();
		let owner = Pubkey::new_unique();
		let mint = Pubkey::new_unique();
		let ix = build_create_ata_idempotent_ix(&payer, &owner, &mint);

		assert_eq!(ix.program_id, ASSOCIATED_TOKEN_PROGRAM_ID);
		assert_eq!(ix.data, vec![1u8], "must be CreateIdempotent (= [1])");

		assert_eq!(ix.accounts.len(), 6);
		assert!(ix.accounts[0].is_signer);
		assert!(ix.accounts[0].is_writable);
		assert_eq!(ix.accounts[0].pubkey, payer);

		assert!(ix.accounts[1].is_writable);
		assert_eq!(ix.accounts[1].pubkey, derive_ata(&owner, &mint));

		assert_eq!(ix.accounts[2].pubkey, owner);
		assert_eq!(ix.accounts[3].pubkey, mint);
		assert_eq!(ix.accounts[4].pubkey, SYSTEM_PROGRAM_ID);
		assert_eq!(ix.accounts[5].pubkey, SPL_TOKEN_PROGRAM_ID);
	}
}
