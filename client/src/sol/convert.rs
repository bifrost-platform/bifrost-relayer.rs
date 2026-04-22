// SPDX-License-Identifier: Apache-2.0
//
// EVM `Socket_Message` ↔ cccp-solana borsh mirror conversions. Used by
// `crate::eth::handlers::socket_relay_handler::dispatch_to_solana` to
// convert a Bifrost-decoded outbound message into the byte-identical
// `PollSubmit` shape the on-chain `cccp-solana::poll(...)` instruction
// expects.

use solana_sdk::pubkey::Pubkey;

use br_primitives::contracts::socket::Socket_Struct::{Round_Up_Submit, Socket_Message};

use crate::sol::codec::{
	AssetIndex, ChainIndex, Instruction, PollSubmit, RBCmethod, RequestId, RoundUpSubmit,
	Signatures, SocketMessage, TaskParams,
};

/// Convert a Bifrost-side `Socket_Message` into the borsh-encodable
/// `PollSubmit` shape. The `option` slot is filled with zeros — the
/// CCCP protocol does not currently use it for the Solana track.
///
/// The mapping is mechanical: every field has a 1:1 EVM ↔ borsh
/// counterpart, byte-pinned by the cross-impl tests in
/// `crate::sol::codec::tests::cross_impl_request_id_pack` and the
/// matching test on the cccp-solana side.
pub fn build_sol_poll_submit(msg: &Socket_Message) -> eyre::Result<PollSubmit> {
	Ok(PollSubmit {
		msg: socket_message_to_borsh(msg)?,
		sigs: empty_signatures(),
		option: [0u8; 32],
	})
}

/// Lift the EVM `Socket_Message` into the borsh-mirror form. The
/// signatures field is populated separately by the caller (the
/// outbound handler) once it has collected the per-relayer secp256k1
/// signatures.
pub fn socket_message_to_borsh(msg: &Socket_Message) -> eyre::Result<SocketMessage> {
	let amount = msg.params.amount.to_be_bytes::<32>();

	Ok(SocketMessage {
		req_id: RequestId {
			chain: ChainIndex(chain_index_bytes(&msg.req_id.ChainIndex)?),
			round_id: msg.req_id.round_id,
			sequence: msg.req_id.sequence,
		},
		status: msg.status,
		ins_code: Instruction {
			chain: ChainIndex(chain_index_bytes(&msg.ins_code.ChainIndex)?),
			method: RBCmethod(rbc_method_bytes(&msg.ins_code.RBCmethod)?),
		},
		params: TaskParams {
			token_idx0: AssetIndex(msg.params.tokenIDX0.into()),
			token_idx1: AssetIndex(msg.params.tokenIDX1.into()),
			refund: msg.params.refund.into(),
			to: msg.params.to.into(),
			amount,
			variants: msg.params.variants.to_vec(),
		},
	})
}

/// Empty signature container. The actual r/s/v values are filled in by
/// the relayer once consensus has gathered enough secp256k1 signatures.
fn empty_signatures() -> Signatures {
	Signatures::default()
}

fn chain_index_bytes(value: &alloy::primitives::FixedBytes<4>) -> eyre::Result<[u8; 4]> {
	Ok((*value).into())
}

fn rbc_method_bytes(value: &alloy::primitives::FixedBytes<16>) -> eyre::Result<[u8; 16]> {
	Ok((*value).into())
}

/// Convert a Bifrost-side `Round_Up_Submit` into the borsh-encodable
/// `RoundUpSubmit` shape consumed by the on-chain `cccp-solana::
/// round_control_relay(...)` instruction.
///
/// Field mapping (byte-pinned by the round-trip test below and by the
/// on-chain layout in `cccp-solana::codec::RoundUpSubmit`):
///
///   * `round`        = `U256::to_be_bytes::<32>()` — matches the
///     on-chain `round_as_u64` extractor, which reads the low 8 bytes
///     of the uint256 as a big-endian `u64`.
///   * `new_relayers` = each EVM `Address` becomes a `[u8; 20]` in the
///     same order. The on-chain program does not sort — it verifies the
///     signatures against this exact ordering, so the caller MUST have
///     already sorted the relayer set the same way the on-chain majority
///     check expects (ascending by address bytes).
///   * `sigs`         = `{ r, s, v }` copied across; `r`/`s` are
///     `FixedBytes<32>` so they convert to `[u8; 32]`; `v` is
///     `alloy::primitives::Bytes` which derefs to `[u8]`.
///
/// Returns an error if the three signature vectors have mismatched
/// lengths — that would mean the upstream `get_sorted_signatures`
/// produced a malformed bundle and we refuse to build a doomed IX.
pub fn build_sol_round_up_submit(submit: &Round_Up_Submit) -> eyre::Result<RoundUpSubmit> {
	let round = submit.round.to_be_bytes::<32>();

	let new_relayers: Vec<[u8; 20]> =
		submit.new_relayers.iter().map(|addr| (*addr).into()).collect();

	let r_len = submit.sigs.r.len();
	let s_len = submit.sigs.s.len();
	let v_len = submit.sigs.v.len();
	if r_len != s_len || s_len != v_len {
		eyre::bail!("mismatched signature vectors: r={r_len} s={s_len} v={v_len}");
	}

	let r: Vec<[u8; 32]> = submit.sigs.r.iter().map(|x| (*x).into()).collect();
	let s: Vec<[u8; 32]> = submit.sigs.s.iter().map(|x| (*x).into()).collect();
	let v: Vec<u8> = submit.sigs.v.iter().copied().collect();

	Ok(RoundUpSubmit { round, new_relayers, sigs: Signatures { r, s, v } })
}

/// Extract the depositor's Solana wallet pubkey from the
/// `Task_Params.variants` payload.
///
/// The CCCP `Task_Params.to` slot is only 20 bytes wide (= EVM address),
/// which can't carry a 32-byte Solana `Pubkey`. The convention used by
/// the cccp-solana `request` IX (and by any non-cccp tooling that emits
/// CCCP messages targeting Solana) is to embed the depositor's Solana
/// wallet in the **first 32 bytes** of `variants`. Anything past that
/// is treated as opaque payload.
///
/// Returns an error if the slice is shorter than 32 bytes; in that case
/// the dispatch path logs and skips the message rather than crashing.
pub fn decode_solana_wallet_from_variants(variants: &[u8]) -> eyre::Result<Pubkey> {
	if variants.len() < 32 {
		eyre::bail!(
			"variants payload too short ({} bytes); expected >= 32 to carry a Solana wallet",
			variants.len()
		);
	}
	let mut bytes = [0u8; 32];
	bytes.copy_from_slice(&variants[..32]);
	Ok(Pubkey::new_from_array(bytes))
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloy::primitives::{Address, Bytes, FixedBytes, U256};
	use br_primitives::contracts::socket::Socket_Struct::{
		Instruction as EvmInstruction, RequestID, Task_Params,
	};

	fn sample_evm_socket_message() -> Socket_Message {
		Socket_Message {
			req_id: RequestID {
				ChainIndex: FixedBytes::from([0x00, 0x00, 0x0b, 0xfc]),
				round_id: 7,
				sequence: 42,
			},
			status: 5,
			ins_code: EvmInstruction {
				ChainIndex: FixedBytes::from([b'S', b'O', b'L', 0]),
				RBCmethod: FixedBytes::from([
					0x02, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
					0x00, 0x00, 0x00,
				]),
			},
			params: Task_Params {
				tokenIDX0: FixedBytes::from([
					0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
					0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
					0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
				]),
				tokenIDX1: FixedBytes::from([0u8; 32]),
				refund: Address::from([0x11; 20]),
				to: Address::from([0x22; 20]),
				amount: U256::from(1_000_000u64),
				variants: Bytes::from(b"hello".to_vec()),
			},
		}
	}

	#[test]
	fn decode_solana_wallet_from_variants_happy_path() {
		let mut variants = vec![0xab; 32];
		variants.extend_from_slice(b"trailing payload");
		let pk = decode_solana_wallet_from_variants(&variants).unwrap();
		assert_eq!(pk, Pubkey::new_from_array([0xab; 32]));
	}

	#[test]
	fn decode_solana_wallet_from_variants_too_short() {
		let variants = vec![0u8; 16];
		assert!(decode_solana_wallet_from_variants(&variants).is_err());
	}

	#[test]
	fn round_up_submit_to_borsh_preserves_all_fields() {
		use br_primitives::contracts::socket::Socket_Struct::Signatures as EvmSignatures;

		let evm = Round_Up_Submit {
			round: U256::from(42u64),
			new_relayers: vec![Address::from([0x11; 20]), Address::from([0x22; 20])],
			sigs: EvmSignatures {
				r: vec![FixedBytes::from([0xaa; 32]), FixedBytes::from([0xbb; 32])],
				s: vec![FixedBytes::from([0xcc; 32]), FixedBytes::from([0xdd; 32])],
				v: Bytes::from(vec![27u8, 28u8]),
			},
		};

		let borsh = build_sol_round_up_submit(&evm).expect("conversion");

		// round = uint256 BE; last 8 bytes must encode the u64 value so the
		// on-chain `round_as_u64` extractor recovers the same number.
		assert_eq!(&borsh.round[0..24], &[0u8; 24]);
		assert_eq!(&borsh.round[24..32], &42u64.to_be_bytes());

		assert_eq!(borsh.new_relayers, vec![[0x11u8; 20], [0x22u8; 20]]);
		assert_eq!(borsh.sigs.r, vec![[0xaau8; 32], [0xbbu8; 32]]);
		assert_eq!(borsh.sigs.s, vec![[0xccu8; 32], [0xddu8; 32]]);
		assert_eq!(borsh.sigs.v, vec![27u8, 28u8]);

		// Round-trip through borsh to guarantee wire compatibility.
		let encoded = borsh::to_vec(&borsh).expect("borsh serialize");
		let decoded = <RoundUpSubmit as borsh::BorshDeserialize>::try_from_slice(&encoded)
			.expect("borsh deserialize");
		assert_eq!(decoded, borsh);
	}

	#[test]
	fn round_up_submit_rejects_mismatched_sig_lengths() {
		use br_primitives::contracts::socket::Socket_Struct::Signatures as EvmSignatures;

		let evm = Round_Up_Submit {
			round: U256::from(1u64),
			new_relayers: vec![Address::from([0u8; 20])],
			sigs: EvmSignatures {
				r: vec![FixedBytes::from([0u8; 32]), FixedBytes::from([0u8; 32])],
				s: vec![FixedBytes::from([0u8; 32])],
				v: Bytes::from(vec![27u8]),
			},
		};

		assert!(build_sol_round_up_submit(&evm).is_err());
	}

	#[test]
	fn evm_to_borsh_round_trip_matches_canonical_layout() {
		let evm = sample_evm_socket_message();
		let borsh = socket_message_to_borsh(&evm).expect("conversion");
		let encoded = borsh::to_vec(&borsh).expect("borsh");
		// Same 194-byte expectation as the cccp-solana cross-impl
		// fixture (`socket_message_borsh_layout_matches_relayer_fixture`)
		// and the relayer-side codec roundtrip
		// (`socket_message_borsh_roundtrip`).
		assert_eq!(encoded.len(), 194);

		// Spot-check the structurally important fields.
		assert_eq!(borsh.req_id.chain.0, [0x00, 0x00, 0x0b, 0xfc]);
		assert_eq!(borsh.req_id.round_id, 7);
		assert_eq!(borsh.req_id.sequence, 42);
		assert_eq!(borsh.status, 5);
		assert_eq!(borsh.ins_code.chain.0, [b'S', b'O', b'L', 0]);
		assert_eq!(borsh.params.refund, [0x11; 20]);
		assert_eq!(borsh.params.to, [0x22; 20]);
		assert_eq!(&borsh.params.amount[24..32], &1_000_000u64.to_be_bytes());
		assert_eq!(borsh.params.variants, b"hello".to_vec());
	}
}
