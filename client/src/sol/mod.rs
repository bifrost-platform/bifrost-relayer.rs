// SPDX-License-Identifier: Apache-2.0
//
// Solana side of the CCCP relayer. Mirrors the layout of `client/src/btc/`:
//
//   * `client.rs`        — `SolClient` wrapping a nonblocking `RpcClient`
//                          plus a handle to the Bifrost `EthClient`.
//   * `slot_manager.rs`  — slot polling loop, broadcasts `SolEventMessage`
//                          (BTC `BlockManager` analogue). Currently a stub
//                          that documents the wiring; the actual program-
//                          log decoder lives in `handlers/event_decoder.rs`.
//   * `pda.rs`           — pure helpers that derive every PDA the
//                          on-chain `cccp-solana` program expects. Used by
//                          both `handlers::outbound` and the off-chain
//                          tooling.
//   * `handlers/`        — inbound (Solana → Bifrost) and outbound
//                          (Bifrost → Solana) workers, parallel to the
//                          BTC handlers module.

pub mod ata;
pub mod client;
pub mod codec;
pub mod convert;
pub mod decoder;
pub mod ix_builder;
pub mod pda;
pub mod registry;
pub mod slot_manager;

pub mod handlers;

pub const LOG_TARGET: &str = "solana";
pub const SUB_LOG_TARGET: &str = "sol-client";

/// Stable representation of the on-chain `cccp-solana` PDA seed strings.
/// These MUST stay in sync with `cccp-solana::constants::SEED_*` — any
/// drift here will silently break PDA derivation, so we keep them as
/// constants the relayer can compile-time check against the on-chain
/// definitions.
pub mod seeds {
	pub const SOCKET_CONFIG: &[u8] = b"socket";
	pub const VAULT_CONFIG: &[u8] = b"vault";
	pub const ROUND_INFO: &[u8] = b"round";
	pub const REQUEST_RECORD: &[u8] = b"req";
	pub const POLL_SIGS: &[u8] = b"poll";
	pub const POLL_FILTER: &[u8] = b"filter";
	pub const ROUNDUP_HIST: &[u8] = b"rup";
	pub const ROUNDUP_FILTER: &[u8] = b"rupf";
	pub const ASSET_CONFIG: &[u8] = b"asset";
	pub const USER_REGISTRY: &[u8] = b"user";
}
