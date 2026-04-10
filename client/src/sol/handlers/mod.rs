// SPDX-License-Identifier: Apache-2.0
//
// Solana handler workers. Mirror of `client/src/btc/handlers/`. Both
// directions are scaffolded as stubs — the full Anchor IDL decoder
// (inbound) and the `poll(...)` instruction builder (outbound) are the
// next two iterations.

pub mod inbound;
pub mod outbound;
