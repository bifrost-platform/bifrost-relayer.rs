// SPDX-License-Identifier: Apache-2.0
//
// Solana handler workers. Mirror of `client/src/btc/handlers/`:
//
//   * `inbound`  — user-originated `request(...)` events on this cluster,
//     forwarded as unsigned Bifrost extrinsics.
//   * `outbound` — unified worker that both submits BFC → Solana IXs
//     (`poll(...)` / `round_control_relay(...)`) and mirrors Solana →
//     BFC outbound-commit events (`Accepted` / `Rejected`) back to the
//     Bifrost substrate pipeline.

pub mod inbound;
pub mod outbound;
