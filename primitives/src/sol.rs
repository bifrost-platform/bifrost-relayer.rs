// SPDX-License-Identifier: Apache-2.0
//
// Solana primitives mirroring `primitives/src/btc.rs`. The Solana track of
// CCCP follows the same passive-observer pattern as the Bitcoin track:
// the relayer polls slot N, parses program logs from the cccp-solana
// Anchor program, broadcasts an `SolEventMessage` over a tokio channel,
// and lets dedicated `SolInboundHandler` / `SolOutboundHandler` workers
// vote-and-submit the resulting Bifrost extrinsics.
//
// `Event` here is the off-chain decoded form of an Anchor `SocketEvent`
// emitted by the on-chain `cccp-solana` program. The actual decoding is
// performed by `client/src/sol/decoder.rs`; this file only defines the
// wire types.

use serde::{Deserialize, Serialize};

/// 4-byte CCCP `ChainIndex` mirror of cccp-solana's `ChainIndex` newtype.
/// Solana uses `0x534F4C00` ("SOL\0") as a self-documenting placeholder.
pub type ChainIndex = [u8; 4];

/// 32-byte CCCP `AssetIndex` mirror of cccp-solana's `AssetIndex`.
pub type AssetIndex = [u8; 32];

/// 16-byte `RBCmethod` mirror.
pub type RBCmethod = [u8; 16];

/// EVM 20-byte address — `Task_Params.refund` / `Task_Params.to`.
pub type EvmAddress = [u8; 20];

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
/// A Solana program event type, decoded from a `SocketEvent` log line.
pub enum EventType {
	/// User-side `request` IX emitted `Socket(Requested)`.
	Inbound,
	/// Relayer outbound flow emitted `Socket(Executed)` etc.
	Outbound,
	/// New slot heartbeat (no events in this slot).
	NewSlot,
}

#[derive(Debug, Clone)]
/// One CCCP socket message decoded from a confirmed Solana transaction.
///
/// This is the Solana counterpart to `bp-cccp::SocketMessage` and is
/// produced by walking program logs of confirmed transactions touching
/// the cccp-solana program ID.
pub struct Event {
	/// Solana transaction signature (= 64 byte tx id) as base58 string.
	pub signature: String,
	/// Slot in which the transaction was confirmed.
	pub slot: u64,
	/// Decoded `RequestId` chain bytes (= the Solana ChainIndex for inbound,
	/// or BFC_MAIN for outbound).
	pub req_chain: ChainIndex,
	/// `RequestId.round_id`.
	pub round_id: u64,
	/// `RequestId.sequence`.
	pub sequence: u128,
	/// Phase the message advanced to (e.g. `Requested` for inbound,
	/// `Committed` for inbound-commit, `Executed` for outbound-exec).
	pub status: u8,
	/// `SocketMessage.ins_code.chain` (4-byte `ChainIndex`). Preserved
	/// verbatim from the on-chain event so every relayer's re-encoded
	/// `Socket_Message` hashes byte-identically — required for BFC-side
	/// quorum to form on the submitted extrinsic.
	pub ins_code_chain: ChainIndex,
	/// `SocketMessage.ins_code.method` (16-byte `RBCmethod`). Same
	/// quorum-equivalence requirement as `ins_code_chain`.
	pub ins_code_method: RBCmethod,
	/// Asset index from `Task_Params.tokenIDX0`.
	pub asset_index: AssetIndex,
	/// Recipient EVM address from `Task_Params.to`.
	pub to: EvmAddress,
	/// Refund EVM address from `Task_Params.refund` (after the on-chain
	/// `request` IX has overwritten it with the registered user address).
	pub refund: EvmAddress,
	/// Amount as 32-byte big-endian `uint256`.
	pub amount: [u8; 32],
	/// Variants payload (`Task_Params.variants`), if any.
	pub variants: Vec<u8>,
}

#[derive(Debug, Clone)]
/// Channel-broadcast message for slot polling. Mirrors the BTC
/// `EventMessage` shape so the same orchestration patterns apply.
pub struct EventMessage {
	/// Slot the events were sourced from.
	pub slot: u64,
	/// What kind of slot this is — used by handlers to filter.
	pub event_type: EventType,
	/// Decoded events; empty for `NewSlot` heartbeats.
	pub events: Vec<Event>,
}

impl EventMessage {
	pub fn new(slot: u64, event_type: EventType, events: Vec<Event>) -> Self {
		Self { slot, event_type, events }
	}

	pub fn inbound(slot: u64) -> Self {
		Self::new(slot, EventType::Inbound, vec![])
	}

	pub fn outbound(slot: u64) -> Self {
		Self::new(slot, EventType::Outbound, vec![])
	}

	pub fn new_slot(slot: u64) -> Self {
		Self::new(slot, EventType::NewSlot, vec![])
	}
}
