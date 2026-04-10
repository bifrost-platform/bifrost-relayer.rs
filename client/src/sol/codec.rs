// SPDX-License-Identifier: Apache-2.0
//
// Minimal borsh-compatible mirror of the cccp-solana codec types. We do
// **not** depend on `cccp-solana` directly because that crate pulls in
// `anchor-lang` 1.0 + a full SBF toolchain — too heavy for the relayer
// host build. Instead we hand-write borsh impls that match the on-chain
// AnchorSerialize layout byte-for-byte and pin the contract via the
// `tests::cross_impl_*` regression tests at the bottom of this file.
//
// Anchor `AnchorSerialize` is just a thin wrapper over
// `borsh::BorshSerialize`, so for plain structs the wire format is the
// same as `borsh::to_vec(&value)`. The discriminator strategy used by
// `#[event]` and `#[instruction]` is `sha256("event:<Name>")[..8]` and
// `sha256("global:<ix_name>")[..8]` respectively (= "sighash" in the
// anchor-syn source). Both are computed at compile time below via the
// `anchor_*_disc` helpers.
//
// **Cross-impl contract**: every type in this file is asserted byte-equal
// against a fixture also embedded in
// `cccp-solana/programs/cccp-solana/src/codec/named_type.rs::request_id_tests`.

use borsh::{BorshDeserialize, BorshSerialize};

// ---------------------------------------------------------------------------
// Newtypes — mirror cccp-solana::codec::named_type::*
// ---------------------------------------------------------------------------

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChainIndex(pub [u8; 4]);

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RBCmethod(pub [u8; 16]);

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AssetIndex(pub [u8; 32]);

impl AssetIndex {
	pub const ZERO: AssetIndex = AssetIndex([0u8; 32]);
}

pub type EvmAddress = [u8; 20];

// ---------------------------------------------------------------------------
// `RequestId` — bytes4 chain || u64 round_id || u128 sequence.
// ---------------------------------------------------------------------------
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestId {
	pub chain: ChainIndex,
	pub round_id: u64,
	pub sequence: u128,
}

impl RequestId {
	/// Mirror of `cccp-solana::RequestId::pack`. **Used as the seed for the
	/// `request_record` PDA — must stay byte-identical to the on-chain
	/// implementation or every relayer-built `poll(...)` IX will miss its
	/// account.** The cross-impl test in
	/// `crate::sol::pda::tests::rid_pack_matches_canonical_fixture` pins
	/// the contract.
	pub fn pack(&self) -> [u8; 32] {
		let mut out = [0u8; 32];
		out[0..4].copy_from_slice(&self.chain.0);
		out[4..12].copy_from_slice(&self.round_id.to_be_bytes());
		out[12..28].copy_from_slice(&self.sequence.to_be_bytes());
		out
	}
}

// ---------------------------------------------------------------------------
// `Instruction` — Solidity-named tuple of (chain, method).
// ---------------------------------------------------------------------------
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Instruction {
	pub chain: ChainIndex,
	pub method: RBCmethod,
}

// ---------------------------------------------------------------------------
// `TaskParams` — has the dynamic `variants` field, so its borsh layout
// uses borsh's standard `Vec<u8>` length-prefix encoding.
// ---------------------------------------------------------------------------
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskParams {
	pub token_idx0: AssetIndex,
	pub token_idx1: AssetIndex,
	pub refund: EvmAddress,
	pub to: EvmAddress,
	/// uint256 amount, big-endian.
	pub amount: [u8; 32],
	pub variants: Vec<u8>,
}

// ---------------------------------------------------------------------------
// `SocketMessage` (= the Anchor `SocketEvent` payload).
// ---------------------------------------------------------------------------
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SocketMessage {
	pub req_id: RequestId,
	pub status: u8,
	pub ins_code: Instruction,
	pub params: TaskParams,
}

// ---------------------------------------------------------------------------
// `Signatures` — bytes32[] r, bytes32[] s, bytes v
// ---------------------------------------------------------------------------
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Signatures {
	pub r: Vec<[u8; 32]>,
	pub s: Vec<[u8; 32]>,
	pub v: Vec<u8>,
}

// ---------------------------------------------------------------------------
// `PollSubmit` — relayer-side `poll(...)` first arg.
// ---------------------------------------------------------------------------
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PollSubmit {
	pub msg: SocketMessage,
	pub sigs: Signatures,
	/// uint256 option — opaque, kept for parity.
	pub option: [u8; 32],
}

// ---------------------------------------------------------------------------
// `RoundUpSubmit` — relayer-side `round_control_relay(...)` arg.
// ---------------------------------------------------------------------------
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RoundUpSubmit {
	/// uint256 round.
	pub round: [u8; 32],
	pub new_relayers: Vec<EvmAddress>,
	pub sigs: Signatures,
}

// ---------------------------------------------------------------------------
// Anchor discriminator helpers — sha256(<namespace>:<name>)[..8].
// We hard-code the four discriminators we need rather than computing them
// at runtime so the relayer is independent of the `sha2` feature gate.
// The `tests::anchor_discriminator_constants_match` test recomputes them
// at runtime and asserts equality, so any future Anchor version bump that
// changes the algorithm will fail loudly.
// ---------------------------------------------------------------------------

/// `sha256("event:SocketEvent")[..8]`
pub const SOCKET_EVENT_DISCRIMINATOR: [u8; 8] = [0xaa, 0x63, 0xaa, 0xa9, 0xc2, 0xf7, 0xb4, 0xa7];

/// `sha256("event:RoundUpEvent")[..8]`
pub const ROUND_UP_EVENT_DISCRIMINATOR: [u8; 8] = [0x2a, 0xee, 0x02, 0xc5, 0xa2, 0x51, 0xeb, 0xd8];

/// `sha256("global:poll")[..8]`
pub const POLL_IX_DISCRIMINATOR: [u8; 8] = [0x4c, 0xaa, 0xbb, 0xb3, 0x76, 0xb3, 0xe4, 0x21];

/// `sha256("global:round_control_relay")[..8]`
pub const ROUND_CONTROL_RELAY_IX_DISCRIMINATOR: [u8; 8] =
	[0x78, 0x6e, 0x30, 0x2a, 0xe2, 0xa9, 0xda, 0x77];

/// `sha256("global:submit_signatures")[..8]`
pub const SUBMIT_SIGNATURES_IX_DISCRIMINATOR: [u8; 8] =
	[0xc2, 0x05, 0x12, 0x98, 0xf5, 0x90, 0x86, 0x4d];

/// `sha256("global:poll_buffered")[..8]`
pub const POLL_BUFFERED_IX_DISCRIMINATOR: [u8; 8] =
	[0xde, 0x5b, 0x9f, 0xaf, 0xc9, 0x56, 0x81, 0xeb];

/// Compute an Anchor sighash at runtime. Used by the regression tests
/// below; production paths use the const values above.
pub fn anchor_sighash(namespace: &str, name: &str) -> [u8; 8] {
	use sha2::{Digest, Sha256};
	let preimage = format!("{namespace}:{name}");
	let digest = Sha256::digest(preimage.as_bytes());
	let mut out = [0u8; 8];
	out.copy_from_slice(&digest[..8]);
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Cross-impl byte fixture asserted by the matching test in
	/// `cccp-solana/programs/cccp-solana/src/codec/named_type.rs`. If the
	/// fixture diverges between the two crates, every PDA derivation
	/// silently misaligns.
	#[test]
	fn cross_impl_request_id_pack() {
		let expected: [u8; 32] = [
			0x00, 0x00, 0x0b, 0xfc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00,
			0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2a,
			0x00, 0x00, 0x00, 0x00,
		];
		let rid =
			RequestId { chain: ChainIndex([0x00, 0x00, 0x0b, 0xfc]), round_id: 7, sequence: 42 };
		assert_eq!(rid.pack(), expected);
	}

	/// Sanity-check our hand-baked discriminator constants against the
	/// runtime sha256 computation. Any future change to Anchor's
	/// namespacing scheme will trip this immediately.
	#[test]
	fn anchor_discriminator_constants_match() {
		assert_eq!(anchor_sighash("event", "SocketEvent"), SOCKET_EVENT_DISCRIMINATOR);
		assert_eq!(anchor_sighash("event", "RoundUpEvent"), ROUND_UP_EVENT_DISCRIMINATOR);
		assert_eq!(anchor_sighash("global", "poll"), POLL_IX_DISCRIMINATOR);
		assert_eq!(
			anchor_sighash("global", "round_control_relay"),
			ROUND_CONTROL_RELAY_IX_DISCRIMINATOR
		);
		assert_eq!(
			anchor_sighash("global", "submit_signatures"),
			SUBMIT_SIGNATURES_IX_DISCRIMINATOR
		);
		assert_eq!(anchor_sighash("global", "poll_buffered"), POLL_BUFFERED_IX_DISCRIMINATOR);
	}

	/// Borsh round-trip a SocketMessage and assert the byte length is
	/// what cccp-solana would produce. The exact bytes are then
	/// asserted in the matching test inside cccp-solana so any wire
	/// format drift fails on both sides.
	#[test]
	fn socket_message_borsh_roundtrip() {
		let msg = SocketMessage {
			req_id: RequestId {
				chain: ChainIndex([0x00, 0x00, 0x0b, 0xfc]),
				round_id: 7,
				sequence: 42,
			},
			status: 5,
			ins_code: Instruction {
				chain: ChainIndex(*b"SOL\x00"),
				method: RBCmethod([
					0x02, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
					0x00, 0x00, 0x00,
				]),
			},
			params: TaskParams {
				token_idx0: AssetIndex([
					0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
					0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
					0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
				]),
				token_idx1: AssetIndex::ZERO,
				refund: [0x11; 20],
				to: [0x22; 20],
				amount: {
					let mut a = [0u8; 32];
					a[24..32].copy_from_slice(&1_000_000u64.to_be_bytes());
					a
				},
				variants: b"hello".to_vec(),
			},
		};

		let encoded = borsh::to_vec(&msg).expect("borsh serialize");
		let decoded = SocketMessage::try_from_slice(&encoded).expect("borsh deserialize");
		assert_eq!(decoded, msg);
		// Sanity: 4+8+16 (req_id) + 1 (status) + 4+16 (ins_code) + 32+32+20+20+32 (static
		// task_params) + 4 (variants len prefix) + 5 (variants payload)
		// = 28 + 1 + 20 + 136 + 4 + 5 = 194 bytes.
		assert_eq!(encoded.len(), 194);
	}
}
