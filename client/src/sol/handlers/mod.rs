// SPDX-License-Identifier: Apache-2.0
//
// Solana handler workers:
//   * `outbound`     — BFC → Solana IX submission + Solana → BFC commit
//                      mirror (relays `Executed`/`Reverted` to BFC's Socket).
//   * `queue_poller` — Solana → BFC ingestion: `on_flight_poll` /
//                      `finalize_poll` into `cccp-relay-queue`.

pub mod outbound;
pub mod queue_poller;
