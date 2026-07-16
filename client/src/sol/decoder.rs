// SPDX-License-Identifier: Apache-2.0
//
// Anchor program-log decoder. `emit!(SocketEvent { ... })` writes a line of
// the form
//
//   Program data: <base64>
//
// where the base64 payload is `[8B discriminator || borsh-serialized event]`.
// The 8-byte discriminator is `sha256("event:<EventName>")[..8]` (see
// `crate::sol::codec`). This module walks a transaction's `log_messages`
// list, identifies the lines that belong to our program ID, decodes the
// matching `SocketEvent` / `RoundUpEvent` payloads, and converts them
// into the relayer-facing `br_primitives::sol::Event` shape.
//
// We tolerate (and ignore):
//   * lines that are not `Program data: ...`,
//   * lines whose base64 payload is shorter than 8 bytes,
//   * payloads whose discriminator doesn't match any known event.

use base64::Engine;
use borsh::BorshDeserialize;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use crate::sol::codec::{
	ROUND_UP_EVENT_DISCRIMINATOR, RoundUpSubmit, SOCKET_EVENT_DISCRIMINATOR, SocketMessage,
};

const PROGRAM_DATA_PREFIX: &str = "Program data: ";

/// Result of decoding a single program log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedAnchorEvent {
	Socket(SocketMessage),
	RoundUp(RoundUpSubmit),
}

/// Decode only a transaction that committed successfully. Solana retains
/// program logs for failed transactions even though its state was rolled
/// back, so `meta.err` must be carried into this trust boundary.
pub fn decode_successful_anchor_events(
	program_id: &Pubkey,
	transaction_failed: bool,
	log_messages: &[String],
) -> Vec<DecodedAnchorEvent> {
	if transaction_failed {
		return Vec::new();
	}
	decode_anchor_events(program_id, log_messages)
}

/// Walk `Program data: <base64>` lines emitted while `program_id` is the
/// active invocation and decode SocketEvent / RoundUpEvent payloads.
/// Runtime invocation/success/failure markers maintain a CPI stack so an
/// unrelated program cannot forge a CCCP event by reusing its discriminator.
pub fn decode_anchor_events(
	program_id: &Pubkey,
	log_messages: &[String],
) -> Vec<DecodedAnchorEvent> {
	let mut out = Vec::new();
	let mut invocation_stack: Vec<Pubkey> = Vec::new();
	for line in log_messages {
		if let Some(invoked) = invoked_program(line) {
			invocation_stack.push(invoked);
			continue;
		}
		if let Some(completed) = completed_program(line) {
			if invocation_stack.last() == Some(&completed) {
				invocation_stack.pop();
			}
			continue;
		}

		if invocation_stack.last() != Some(program_id) {
			continue;
		}
		let Some(payload_b64) = line.strip_prefix(PROGRAM_DATA_PREFIX) else {
			continue;
		};
		let payload_b64 = payload_b64.trim();
		let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload_b64) else {
			continue;
		};
		if bytes.len() < 8 {
			continue;
		}
		let (disc, body) = bytes.split_at(8);
		if disc == SOCKET_EVENT_DISCRIMINATOR {
			if let Ok(msg) = SocketMessage::try_from_slice(body) {
				out.push(DecodedAnchorEvent::Socket(msg));
			}
		} else if disc == ROUND_UP_EVENT_DISCRIMINATOR
			&& let Ok(submit) = RoundUpSubmit::try_from_slice(body)
		{
			out.push(DecodedAnchorEvent::RoundUp(submit));
		}
	}
	out
}

fn invoked_program(line: &str) -> Option<Pubkey> {
	let mut parts = line.strip_prefix("Program ")?.split_whitespace();
	let id = parts.next()?;
	(parts.next()? == "invoke").then(|| Pubkey::from_str(id).ok()).flatten()
}

fn completed_program(line: &str) -> Option<Pubkey> {
	let mut parts = line.strip_prefix("Program ")?.split_whitespace();
	let id = parts.next()?;
	let status = parts.next()?;
	(status == "success" || status.starts_with("failed:"))
		.then(|| Pubkey::from_str(id).ok())
		.flatten()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::sol::codec::{
		AssetIndex, ChainIndex, Instruction, RBCmethod, RequestId, TaskParams,
	};

	fn synth_socket_event_log_line(msg: &SocketMessage) -> String {
		let body = borsh::to_vec(msg).unwrap();
		let mut payload = Vec::with_capacity(8 + body.len());
		payload.extend_from_slice(&SOCKET_EVENT_DISCRIMINATOR);
		payload.extend_from_slice(&body);
		let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);
		format!("Program data: {encoded}")
	}

	#[test]
	fn decodes_known_socket_event() {
		let program_id = Pubkey::new_unique();
		let msg = SocketMessage {
			req_id: RequestId { chain: ChainIndex(*b"SOL\0"), round_id: 1, sequence: 7 },
			status: 5,
			ins_code: Instruction {
				chain: ChainIndex([0x00, 0x00, 0x0b, 0xfc]),
				method: RBCmethod([
					0x03, 0x01, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
					0x00, 0x00, 0x00,
				]),
			},
			params: TaskParams {
				token_idx0: AssetIndex([0xab; 32]),
				token_idx1: AssetIndex::ZERO,
				refund: [0xcd; 20],
				to: [0xef; 20],
				amount: {
					let mut a = [0u8; 32];
					a[24..32].copy_from_slice(&123_456u64.to_be_bytes());
					a
				},
				variants: vec![1, 2, 3, 4, 5],
			},
		};

		let logs = vec![
			format!("Program {program_id} invoke [1]"),
			"Program log: Instruction: Poll".to_string(),
			synth_socket_event_log_line(&msg),
			format!("Program {program_id} consumed 1234 of 200000 compute units"),
			format!("Program {program_id} success"),
		];

		let decoded = decode_anchor_events(&program_id, &logs);
		assert_eq!(decoded.len(), 1);
		match &decoded[0] {
			DecodedAnchorEvent::Socket(d) => assert_eq!(d, &msg),
			other => panic!("expected SocketEvent, got {other:?}"),
		}
	}

	#[test]
	fn ignores_lines_with_unknown_discriminator() {
		let program_id = Pubkey::new_unique();
		let bogus_payload = {
			let mut buf = Vec::new();
			buf.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x12, 0x34, 0x56, 0x78]);
			buf.extend_from_slice(&[0u8; 32]);
			base64::engine::general_purpose::STANDARD.encode(&buf)
		};
		let logs = vec![
			format!("Program {program_id} invoke [1]"),
			format!("Program data: {bogus_payload}"),
			format!("Program {program_id} success"),
		];
		let decoded = decode_anchor_events(&program_id, &logs);
		assert!(decoded.is_empty(), "unknown discriminator must be ignored");
	}

	#[test]
	fn ignores_non_program_data_lines() {
		let program_id = Pubkey::new_unique();
		let logs = vec![
			"Program log: hello world".to_string(),
			"Program return: ...".to_string(),
			"garbage line".to_string(),
		];
		let decoded = decode_anchor_events(&program_id, &logs);
		assert!(decoded.is_empty());
	}

	#[test]
	fn ignores_forged_event_from_foreign_program() {
		let program_id = Pubkey::new_unique();
		let foreign_program = Pubkey::new_unique();
		let msg = SocketMessage::default();
		let logs = vec![
			format!("Program {foreign_program} invoke [1]"),
			synth_socket_event_log_line(&msg),
			format!("Program {foreign_program} success"),
		];

		assert!(decode_anchor_events(&program_id, &logs).is_empty());
	}

	#[test]
	fn ignores_foreign_cpi_data_while_accepting_cccp_data() {
		let program_id = Pubkey::new_unique();
		let foreign_program = Pubkey::new_unique();
		let foreign_msg = SocketMessage::default();
		let cccp_msg = SocketMessage { status: 5, ..SocketMessage::default() };
		let logs = vec![
			format!("Program {program_id} invoke [1]"),
			format!("Program {foreign_program} invoke [2]"),
			synth_socket_event_log_line(&foreign_msg),
			format!("Program {foreign_program} success"),
			synth_socket_event_log_line(&cccp_msg),
			format!("Program {program_id} success"),
		];

		assert_eq!(
			decode_anchor_events(&program_id, &logs),
			vec![DecodedAnchorEvent::Socket(cccp_msg)]
		);
	}

	#[test]
	fn ignores_valid_event_payload_when_transaction_failed() {
		let program_id = Pubkey::new_unique();
		let msg = SocketMessage::default();
		let logs = vec![
			format!("Program {program_id} invoke [1]"),
			synth_socket_event_log_line(&msg),
			format!("Program {program_id} failed: custom program error: 0x1"),
		];

		assert!(decode_successful_anchor_events(&program_id, true, &logs).is_empty());
		assert_eq!(
			decode_successful_anchor_events(&program_id, false, &logs),
			vec![DecodedAnchorEvent::Socket(msg)]
		);
	}
}
