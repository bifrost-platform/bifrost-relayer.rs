// SPDX-License-Identifier: Apache-2.0
//
// Pure off-chain helpers that derive every `cccp-solana` PDA. The relayer
// uses these to construct outbound `poll(...)` instructions; the on-chain
// program re-derives them from the same seeds during validation, so any
// mismatch fails fast at the `Anchor` constraint check.
//
// All seed strings live in `crate::sol::seeds` and MUST mirror the
// on-chain `cccp-solana::constants::SEED_*` constants exactly.
//
// `RequestId.pack()` mirrors the on-chain `_rid_pack` and is duplicated
// here so the relayer does not need to pull cccp-solana as a dependency
// (which would drag in anchor-lang and a second SBF toolchain).

use solana_sdk::pubkey::Pubkey;

use crate::sol::seeds;

/// 4-byte CCCP `ChainIndex` (mirror of cccp-solana's newtype).
pub type ChainIndex = [u8; 4];

/// 32-byte `AssetIndex`.
pub type AssetIndex = [u8; 32];

/// EVM 20-byte address.
pub type EvmAddress = [u8; 20];

/// On-chain `RequestId` shape: `bytes4 chain || uint64 round_id || uint128 sequence`.
#[derive(Debug, Clone, Copy)]
pub struct RequestId {
	pub chain: ChainIndex,
	pub round_id: u64,
	pub sequence: u128,
}

impl RequestId {
	/// Mirror of `cccp-solana::codec::named_type::RequestId::pack` —
	/// `chain || round_id_be || sequence_be || 4 zero bytes` packed into
	/// a 32-byte block. Used as the `request_record` PDA seed payload.
	pub fn pack(&self) -> [u8; 32] {
		let mut out = [0u8; 32];
		out[0..4].copy_from_slice(&self.chain);
		out[4..12].copy_from_slice(&self.round_id.to_be_bytes());
		out[12..28].copy_from_slice(&self.sequence.to_be_bytes());
		out
	}
}

/// Find a PDA + bump for the given seeds under the cccp-solana program ID.
fn find(seeds: &[&[u8]], program_id: &Pubkey) -> (Pubkey, u8) {
	Pubkey::find_program_address(seeds, program_id)
}

// ---------------------------------------------------------------------------
// Singleton PDAs (the same address regardless of which round / asset)
// ---------------------------------------------------------------------------

pub fn socket_config(program_id: &Pubkey) -> (Pubkey, u8) {
	find(&[seeds::SOCKET_CONFIG], program_id)
}

pub fn vault_config(program_id: &Pubkey) -> (Pubkey, u8) {
	find(&[seeds::VAULT_CONFIG], program_id)
}

// ---------------------------------------------------------------------------
// Per-round PDAs
// ---------------------------------------------------------------------------

pub fn round_info(program_id: &Pubkey, round_id: u64) -> (Pubkey, u8) {
	find(&[seeds::ROUND_INFO, &round_id.to_le_bytes()], program_id)
}

pub fn roundup_hist(program_id: &Pubkey, round_id: u64) -> (Pubkey, u8) {
	find(&[seeds::ROUNDUP_HIST, &round_id.to_le_bytes()], program_id)
}

pub fn roundup_filter(program_id: &Pubkey, round_id: u64, updater: &EvmAddress) -> (Pubkey, u8) {
	find(&[seeds::ROUNDUP_FILTER, &round_id.to_le_bytes(), updater], program_id)
}

// ---------------------------------------------------------------------------
// Per-request PDAs
// ---------------------------------------------------------------------------

pub fn request_record(program_id: &Pubkey, rid: &RequestId) -> (Pubkey, u8) {
	let packed = rid.pack();
	find(&[seeds::REQUEST_RECORD, &packed], program_id)
}

pub fn poll_signatures(program_id: &Pubkey, rid: &RequestId, status: u8) -> (Pubkey, u8) {
	let packed = rid.pack();
	poll_signatures_raw(program_id, &packed, status)
}

/// Variant that accepts a pre-packed `[u8; 32]` rid instead of a
/// `RequestId`. Used by the IX builder which already has the packed form.
pub fn poll_signatures_raw(program_id: &Pubkey, rid_pack: &[u8; 32], status: u8) -> (Pubkey, u8) {
	find(&[seeds::POLL_SIGS, rid_pack, &[status]], program_id)
}

pub fn poll_filter(program_id: &Pubkey, rid: &RequestId, signer: &EvmAddress) -> (Pubkey, u8) {
	let packed = rid.pack();
	find(&[seeds::POLL_FILTER, &packed, signer], program_id)
}

// ---------------------------------------------------------------------------
// Per-asset / per-user PDAs
// ---------------------------------------------------------------------------

pub fn asset_config(program_id: &Pubkey, asset_index: &AssetIndex) -> (Pubkey, u8) {
	find(&[seeds::ASSET_CONFIG, asset_index], program_id)
}

pub fn user_registry(program_id: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
	find(&[seeds::USER_REGISTRY, owner.as_ref()], program_id)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Hex of the expected packed bytes for the canonical fixture used by
	/// the cccp-solana abi_compat tests
	/// (`programs/cccp-solana/tests/abi_compat.rs::fix_canonical`):
	///
	///   chain    = 0x00000bfc  (BFC_MAIN)
	///   round_id = 7           (u64 BE)
	///   sequence = 42          (u128 BE)
	///
	/// Layout: 4B chain || 8B round_id BE || 16B sequence BE || 4B 0 pad.
	/// If this constant ever drifts, both the on-chain `RequestRecord`
	/// PDA seeds AND the relayer-side PDA derivation are misaligned and
	/// every cross-chain message would silently miss its account.
	const CANONICAL_FIXTURE_PACKED: [u8; 32] = [
		0x00, 0x00, 0x0b, 0xfc, // chain
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, // round_id = 7
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x2a, // sequence = 42
		0x00, 0x00, 0x00, 0x00, // tail pad
	];

	#[test]
	fn rid_pack_matches_canonical_fixture() {
		let rid = RequestId { chain: [0x00, 0x00, 0x0b, 0xfc], round_id: 7, sequence: 42 };
		assert_eq!(
			rid.pack(),
			CANONICAL_FIXTURE_PACKED,
			"br-client RequestId.pack() must match cccp-solana's _rid_pack \
             exactly — drift here would corrupt every PDA derivation"
		);
	}

	#[test]
	fn rid_pack_matches_layout() {
		let rid = RequestId { chain: *b"SOL\0", round_id: 7, sequence: 42 };
		let packed = rid.pack();
		assert_eq!(&packed[0..4], b"SOL\0");
		assert_eq!(&packed[4..12], &7u64.to_be_bytes());
		assert_eq!(&packed[12..28], &42u128.to_be_bytes());
		assert_eq!(&packed[28..32], &[0u8; 4]);
	}

	#[test]
	fn pdas_are_deterministic_per_program_id() {
		let pid = Pubkey::new_unique();
		let a = socket_config(&pid).0;
		let b = socket_config(&pid).0;
		assert_eq!(a, b);
	}

	#[test]
	fn singleton_seeds_match_cccp_solana_constants() {
		// These literal byte strings MUST mirror cccp-solana::constants::SEED_*.
		// The test will fail at compile time if any of the imports drift.
		assert_eq!(seeds::SOCKET_CONFIG, b"socket");
		assert_eq!(seeds::VAULT_CONFIG, b"vault");
		assert_eq!(seeds::ROUND_INFO, b"round");
		assert_eq!(seeds::REQUEST_RECORD, b"req");
		assert_eq!(seeds::POLL_SIGS, b"poll");
		assert_eq!(seeds::POLL_FILTER, b"filter");
		assert_eq!(seeds::ROUNDUP_HIST, b"rup");
		assert_eq!(seeds::ROUNDUP_FILTER, b"rupf");
		assert_eq!(seeds::ASSET_CONFIG, b"asset");
		assert_eq!(seeds::USER_REGISTRY, b"user");
	}
}
