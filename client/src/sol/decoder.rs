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

use br_primitives::sol::{Event, EventType};

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

/// Walk every `Program data: <base64>` line in `log_messages` and decode
/// the SocketEvent / RoundUpEvent payloads. Lines from other programs
/// (or unrelated `Program data:` from sub-CPIs) are silently skipped.
pub fn decode_anchor_events(log_messages: &[String]) -> Vec<DecodedAnchorEvent> {
	let mut out = Vec::new();
	for line in log_messages {
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
		} else if disc == ROUND_UP_EVENT_DISCRIMINATOR {
			if let Ok(submit) = RoundUpSubmit::try_from_slice(body) {
				out.push(DecodedAnchorEvent::RoundUp(submit));
			}
		}
	}
	out
}

/// Convert decoded `SocketEvent`s into the relayer-facing `Event` shape.
/// `slot` and `signature` are taken from the surrounding transaction
/// envelope (`getTransaction` response). The split between inbound and
/// outbound is decided by the caller — we only know the message body here.
pub fn into_relayer_events(
	decoded: &[DecodedAnchorEvent],
	slot: u64,
	signature: &str,
) -> (Vec<Event>, Vec<EventType>) {
	let mut events = Vec::new();
	let mut classifications = Vec::new();
	for ev in decoded {
		match ev {
			DecodedAnchorEvent::Socket(msg) => {
				events.push(Event {
					signature: signature.to_string(),
					slot,
					req_chain: msg.req_id.chain.0,
					round_id: msg.req_id.round_id,
					sequence: msg.req_id.sequence,
					status: msg.status,
					asset_index: msg.params.token_idx0.0,
					to: msg.params.to,
					refund: msg.params.refund,
					amount: msg.params.amount,
					variants: msg.params.variants.clone(),
				});
				// Caller will decide inbound vs outbound based on the
				// request_id chain (= this_chain → inbound, otherwise
				// outbound). Default to Inbound here so the slot manager
				// can route correctly.
				classifications.push(EventType::Inbound);
			},
			DecodedAnchorEvent::RoundUp(_) => {
				// Round-up events do not flow through the inbound/outbound
				// event channel; they're handled by a separate worker.
				// Skipping them here keeps the relayer-facing event shape
				// simple.
			},
		}
	}
	(events, classifications)
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
			"Program DrF2... invoke [1]".to_string(),
			"Program log: Instruction: Poll".to_string(),
			synth_socket_event_log_line(&msg),
			"Program DrF2... consumed 1234 of 200000 compute units".to_string(),
		];

		let decoded = decode_anchor_events(&logs);
		assert_eq!(decoded.len(), 1);
		match &decoded[0] {
			DecodedAnchorEvent::Socket(d) => assert_eq!(d, &msg),
			other => panic!("expected SocketEvent, got {other:?}"),
		}

		let (events, classes) = into_relayer_events(&decoded, 999, "sig123");
		assert_eq!(events.len(), 1);
		assert_eq!(events[0].signature, "sig123");
		assert_eq!(events[0].slot, 999);
		assert_eq!(events[0].round_id, 1);
		assert_eq!(events[0].sequence, 7);
		assert_eq!(events[0].status, 5);
		assert_eq!(events[0].refund, [0xcd; 20]);
		assert_eq!(events[0].variants, vec![1, 2, 3, 4, 5]);
		assert_eq!(classes, vec![EventType::Inbound]);
	}

	#[test]
	fn ignores_lines_with_unknown_discriminator() {
		let bogus_payload = {
			let mut buf = Vec::new();
			buf.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x12, 0x34, 0x56, 0x78]);
			buf.extend_from_slice(&[0u8; 32]);
			base64::engine::general_purpose::STANDARD.encode(&buf)
		};
		let logs = vec![format!("Program data: {bogus_payload}")];
		let decoded = decode_anchor_events(&logs);
		assert!(decoded.is_empty(), "unknown discriminator must be ignored");
	}

	#[test]
	fn ignores_non_program_data_lines() {
		let logs = vec![
			"Program log: hello world".to_string(),
			"Program return: ...".to_string(),
			"garbage line".to_string(),
		];
		let decoded = decode_anchor_events(&logs);
		assert!(decoded.is_empty());
	}
}
