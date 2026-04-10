// SPDX-License-Identifier: Apache-2.0
//
// EVM `Socket_Message` ↔ cccp-solana borsh mirror conversions. Used by
// `crate::eth::handlers::socket_relay_handler::dispatch_to_solana` to
// convert a Bifrost-decoded outbound message into the byte-identical
// `PollSubmit` shape the on-chain `cccp-solana::poll(...)` instruction
// expects.

use solana_sdk::pubkey::Pubkey;

use br_primitives::contracts::socket::Socket_Struct::Socket_Message;

use crate::sol::codec::{
	AssetIndex, ChainIndex, Instruction, PollSubmit, RBCmethod, RequestId, Signatures,
	SocketMessage, TaskParams,
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
