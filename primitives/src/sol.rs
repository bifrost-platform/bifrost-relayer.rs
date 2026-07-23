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
pub type ChainIndex = [u8; 4];

/// Solana devnet paired with the Bifrost testbed.
pub const SOL_DEV: ChainIndex = *b"SOL\0";
/// Solana testnet paired with the Bifrost testnet.
pub const SOL_TEST: ChainIndex = *b"SOL\x01";
/// Solana mainnet-beta paired with the Bifrost mainnet.
pub const SOL_MAIN: ChainIndex = *b"SOL\x02";

/// Numeric `ChainId` forms used by relayer routing and BFC registration.
pub const SOL_DEV_CHAIN_ID: u64 = u32::from_be_bytes(SOL_DEV) as u64;
pub const SOL_TEST_CHAIN_ID: u64 = u32::from_be_bytes(SOL_TEST) as u64;
pub const SOL_MAIN_CHAIN_ID: u64 = u32::from_be_bytes(SOL_MAIN) as u64;

/// Immutable identity of one deployed `cccp-solana` program.
///
/// These values are deliberately compiled into the relayer rather than
/// accepted from operator config. A Solana RPC endpoint can therefore never
/// redirect the relayer to a different program or upgrade authority.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SolDeployment {
	/// Upgradeable-loader program account.
	pub program_id: &'static str,
	/// Authority expected on the linked ProgramData account.
	pub upgrade_authority: &'static str,
}

/// ChainId-derived Solana environment identity.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SolEnvironment {
	/// Stable logs/metrics label. This is not operator-configurable.
	pub name: &'static str,
	/// `None` keeps an environment fail-closed until its deployment identity
	/// has been pinned in a relayer release.
	pub deployment: Option<SolDeployment>,
}

/// Bifrost testbed ↔ Solana devnet deployment.
pub const SOL_DEV_ENVIRONMENT: SolEnvironment = SolEnvironment {
	name: "solana-devnet",
	deployment: Some(SolDeployment {
		program_id: "HZ7XCyfXEdkfee2eJfAN6yKEf2S8dVJhmqAcvixKMW5z",
		upgrade_authority: "G6ZrR6V5xMutxTC5z9wkGZqJNCmeA3vxemXWQHRNpxoZ",
	}),
};

/// Bifrost testnet ↔ Solana devnet. Deployment identity will be pinned after
/// this environment is deployed.
pub const SOL_TEST_ENVIRONMENT: SolEnvironment =
	SolEnvironment { name: "solana-devnet", deployment: None };

/// Bifrost mainnet ↔ Solana mainnet-beta. Deployment identity will be pinned
/// only after the production deployment is complete.
pub const SOL_MAIN_ENVIRONMENT: SolEnvironment =
	SolEnvironment { name: "solana", deployment: None };

/// Resolve the immutable environment profile for one canonical Solana
/// `ChainId`. Unknown IDs are not Solana environments.
pub const fn sol_environment(chain_id: u64) -> Option<&'static SolEnvironment> {
	match chain_id {
		SOL_DEV_CHAIN_ID => Some(&SOL_DEV_ENVIRONMENT),
		SOL_TEST_CHAIN_ID => Some(&SOL_TEST_ENVIRONMENT),
		SOL_MAIN_CHAIN_ID => Some(&SOL_MAIN_ENVIRONMENT),
		_ => None,
	}
}

pub fn is_sol_chain_index(index: ChainIndex) -> bool {
	index == SOL_DEV || index == SOL_TEST || index == SOL_MAIN
}

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

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
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
	/// Decoded `RequestId` chain bytes (= SOL_DEV/SOL_TEST/SOL_MAIN for inbound,
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
	/// Asset index from `Task_Params.tokenIDX1` — preserved verbatim from
	/// the on-chain event. cccp-solana hashes the full `User_Request`
	/// (including `token_idx1`) into `RequestRecord.msg_hash` at request
	/// time, so the relayer must keep `token_idx1` byte-identical on every
	/// re-encoding through the round trip; otherwise the source-side
	/// `inbound_commit_relay`'s `request_integrity` check fails with
	/// `UserParamsIntegrity (6025)`.
	pub token_idx1: AssetIndex,
	/// Recipient EVM address from `Task_Params.to`.
	pub to: EvmAddress,
	/// Refund EVM address from `Task_Params.refund`. Flows through verbatim
	/// — the on-chain `request` IX does NOT overwrite it (the old
	/// `register_user` design is gone). Solana-side refund safety instead
	/// binds to `RequestRecord.refund_owner`, a snapshot of the actual
	/// transaction signer's pubkey taken at request time, independent of
	/// whatever this field carries.
	pub refund: EvmAddress,
	/// Amount as 32-byte big-endian `uint256`.
	pub amount: [u8; 32],
	/// Variants payload (`Task_Params.variants`), if any.
	pub variants: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sol_environment_indices_and_chain_ids_are_canonical() {
		assert_eq!(SOL_DEV, *b"SOL\0");
		assert_eq!(SOL_TEST, *b"SOL\x01");
		assert_eq!(SOL_MAIN, *b"SOL\x02");
		assert_eq!(SOL_DEV_CHAIN_ID, 1_397_705_728);
		assert_eq!(SOL_TEST_CHAIN_ID, 1_397_705_729);
		assert_eq!(SOL_MAIN_CHAIN_ID, 1_397_705_730);
		assert_eq!(sol_environment(SOL_DEV_CHAIN_ID), Some(&SOL_DEV_ENVIRONMENT));
		assert_eq!(sol_environment(SOL_TEST_CHAIN_ID), Some(&SOL_TEST_ENVIRONMENT));
		assert_eq!(sol_environment(SOL_MAIN_CHAIN_ID), Some(&SOL_MAIN_ENVIRONMENT));
		assert_eq!(SOL_DEV_ENVIRONMENT.name, "solana-devnet");
		assert_eq!(SOL_TEST_ENVIRONMENT.name, "solana-devnet");
		assert_eq!(SOL_MAIN_ENVIRONMENT.name, "solana");
		assert!(SOL_DEV_ENVIRONMENT.deployment.is_some());
		assert!(SOL_TEST_ENVIRONMENT.deployment.is_none());
		assert!(SOL_MAIN_ENVIRONMENT.deployment.is_none());
		assert!(sol_environment(u64::MAX).is_none());
		assert!(is_sol_chain_index(SOL_DEV));
		assert!(is_sol_chain_index(SOL_TEST));
		assert!(is_sol_chain_index(SOL_MAIN));
		assert!(!is_sol_chain_index(*b"SOL\x03"));
	}
}
