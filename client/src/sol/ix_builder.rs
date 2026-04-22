// SPDX-License-Identifier: Apache-2.0
//
// Off-chain instruction builders for the cccp-solana program. We do NOT
// pull the on-chain crate as a dependency (would drag anchor-lang + a
// full SBF toolchain into the relayer host build). Instead we hand-build
// the instruction data + account-meta lists, pinned against the on-chain
// types via the cross-impl regression tests in `crate::sol::codec` and
// `crate::sol::pda`.
//
// Layout for any Anchor instruction:
//
//   data    = [8B sighash(global, ix_name) || borsh-encoded args]
//   accounts = the order of the `#[derive(Accounts)]` struct fields,
//              with `#[account(mut)]` → writable and `Signer` → signer.

use borsh::BorshSerialize;
use solana_sdk::instruction::{AccountMeta, Instruction as SolanaIx};
use solana_sdk::pubkey::Pubkey;

use crate::sol::codec::{
	AssetIndex, CLOSE_POLL_SIGNATURES_IX_DISCRIMINATOR, POLL_BUFFERED_IX_DISCRIMINATOR,
	POLL_IX_DISCRIMINATOR, PollSubmit, ROUND_CONTROL_RELAY_IX_DISCRIMINATOR, RoundUpSubmit,
	SUBMIT_SIGNATURES_IX_DISCRIMINATOR, Signatures, SocketMessage,
};
use crate::sol::pda;

/// SPL Token program ID — `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`
/// hard-coded as raw bytes so the relayer doesn't need to depend on
/// `spl-token` (which drags its own SBF baggage). Verified at runtime
/// against the canonical base58 string in `tests::token_program_id_const`.
pub const SPL_TOKEN_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
	0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79, 0xac,
	0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff, 0x00, 0xa9,
]);

/// System program ID — `11111111111111111111111111111111` = all-zero pubkey.
pub const SYSTEM_PROGRAM_ID: Pubkey = Pubkey::new_from_array([0u8; 32]);

/// All the per-request token accounts the `poll(...)` IX needs. The
/// caller (= the outbound handler) is responsible for materializing
/// them — for SPL transfers the vault and recipient ATAs must already
/// exist; for inbound rollback the recipient slot points to the user's
/// own ATA.
#[derive(Debug, Clone, Copy)]
pub struct PollTokenAccounts {
	pub mint: Pubkey,
	pub vault_token_account: Pubkey,
	pub recipient_token_account: Pubkey,
}

/// Build the cccp-solana `poll(submit, asset_index)` instruction.
///
/// The relayer must supply:
/// * `program_id`        — the cccp-solana program ID for this cluster
/// * `relayer`           — the Solana fee-payer wallet (= signer of the tx)
/// * `submit`            — the borsh-encodable `PollSubmit` (msg + sigs)
/// * `asset_index`       — `params.token_idx0` mirror; used as a PDA seed for `asset_config`
/// * `tokens`            — vault + recipient + mint addresses
///
/// All PDAs (`socket_config`, `vault_config`, `prev_round`,
/// `request_record`, `asset_config`) are derived internally from
/// `program_id` so the caller cannot pass a wrong account by accident.
pub fn build_poll_ix(
	program_id: &Pubkey,
	relayer: &Pubkey,
	submit: &PollSubmit,
	asset_index: &AssetIndex,
	tokens: &PollTokenAccounts,
) -> SolanaIx {
	let (socket_config, _) = pda::socket_config(program_id);
	let (vault_config, _) = pda::vault_config(program_id);
	let (prev_round, _) = pda::round_info(program_id, submit.msg.req_id.round_id);

	// The on-chain seed uses `submit.msg.req_id.pack()`. Our codec mirror
	// implements an identical `pack()`, pinned by the cross-impl test in
	// `crate::sol::codec::tests::cross_impl_request_id_pack`.
	let rid_pack = submit.msg.req_id.pack();
	let (request_record, _) = pda::request_record(
		program_id,
		&pda::RequestId {
			chain: submit.msg.req_id.chain.0,
			round_id: submit.msg.req_id.round_id,
			sequence: submit.msg.req_id.sequence,
		},
	);
	debug_assert_eq!(
		rid_pack,
		pda::RequestId {
			chain: submit.msg.req_id.chain.0,
			round_id: submit.msg.req_id.round_id,
			sequence: submit.msg.req_id.sequence,
		}
		.pack()
	);

	let (asset_config, _) = pda::asset_config(program_id, &asset_index.0);

	// Account meta order MUST match the on-chain `Poll<'info>` struct
	// (see `cccp-solana/programs/cccp-solana/src/instructions/poll.rs`).
	let accounts = vec![
		AccountMeta::new(*relayer, true),       // relayer (signer, mut)
		AccountMeta::new(socket_config, false), // socket_config (mut, init_if_needed children)
		AccountMeta::new_readonly(vault_config, false), // vault_config
		AccountMeta::new_readonly(prev_round, false), // prev_round
		AccountMeta::new(request_record, false), // request_record (init_if_needed, mut)
		AccountMeta::new_readonly(asset_config, false), // asset_config
		AccountMeta::new_readonly(tokens.mint, false), // mint
		AccountMeta::new(tokens.vault_token_account, false), // vault_token_account (mut)
		AccountMeta::new(tokens.recipient_token_account, false), // recipient_token_account (mut)
		AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false), // token_program
		AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false), // system_program
	];

	// data = discriminator || borsh(submit) || borsh(asset_index)
	let mut data = Vec::with_capacity(8 + 256);
	data.extend_from_slice(&POLL_IX_DISCRIMINATOR);
	submit
		.serialize(&mut data)
		.expect("borsh serialization of PollSubmit must not fail");
	asset_index
		.serialize(&mut data)
		.expect("borsh serialization of AssetIndex must not fail");

	SolanaIx { program_id: *program_id, accounts, data }
}

/// Build the `round_control_relay(submit)` instruction. Used by the round
/// rotation worker to advance the on-chain `latest_round_id` after BFC
/// has committed a new relayer set.
pub fn build_round_control_relay_ix(
	program_id: &Pubkey,
	relayer: &Pubkey,
	submit: &RoundUpSubmit,
) -> SolanaIx {
	let (socket_config, _) = pda::socket_config(program_id);

	// The on-chain `current_round` PDA is keyed by the **current**
	// `latest_round_id` — the relayer must therefore know what the latest
	// round is locally. We accept it as the high-8 bytes of `submit.round`
	// minus 1 (i.e. the round being promoted is `submit.round`, so the
	// current round is `submit.round - 1`).
	let new_round_id = round_id_from_submit(submit);
	let current_round_id = new_round_id.saturating_sub(1);

	let (current_round, _) = pda::round_info(program_id, current_round_id);
	let (new_round, _) = pda::round_info(program_id, new_round_id);

	// Account meta order MUST match the on-chain `RoundControlRelay`
	// struct in `instructions/round_control_relay.rs`.
	let accounts = vec![
		AccountMeta::new(*relayer, true),       // relayer (signer, mut)
		AccountMeta::new(socket_config, false), // socket_config (mut)
		AccountMeta::new_readonly(current_round, false), // current_round
		AccountMeta::new(new_round, false),     // new_round (init, mut)
		AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false), // system_program
	];

	let mut data = Vec::with_capacity(8 + 256);
	data.extend_from_slice(&ROUND_CONTROL_RELAY_IX_DISCRIMINATOR);
	submit
		.serialize(&mut data)
		.expect("borsh serialization of RoundUpSubmit must not fail");

	SolanaIx { program_id: *program_id, accounts, data }
}

/// Maximum number of signatures per `submit_signatures` chunk.
/// With 8 sigs the tx is ~766 bytes — well under 1232.
pub const SIGS_PER_CHUNK: usize = 8;

/// Build a `submit_signatures(sigs, req_id_pack, status)` instruction.
/// The relayer calls this in a loop to fill the `PollSignatures` PDA
/// before sending `poll_buffered`.
pub fn build_submit_signatures_ix(
	program_id: &Pubkey,
	relayer: &Pubkey,
	sigs: &Signatures,
	req_id_pack: &[u8; 32],
	status: u8,
) -> SolanaIx {
	let (socket_config, _) = pda::socket_config(program_id);
	let (poll_sigs, _) = pda::poll_signatures_raw(program_id, req_id_pack, status);

	let accounts = vec![
		AccountMeta::new(*relayer, true), // relayer (signer, mut)
		AccountMeta::new_readonly(socket_config, false), // socket_config
		AccountMeta::new(poll_sigs, false), // poll_signatures (init_if_needed, mut)
		AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false), // system_program
	];

	let mut data = Vec::with_capacity(8 + 256);
	data.extend_from_slice(&SUBMIT_SIGNATURES_IX_DISCRIMINATOR);
	sigs.serialize(&mut data)
		.expect("borsh serialization of Signatures must not fail");
	req_id_pack
		.serialize(&mut data)
		.expect("borsh serialization of req_id_pack must not fail");
	data.push(status);

	SolanaIx { program_id: *program_id, accounts, data }
}

/// Build a `close_poll_signatures(req_id_pack, status)` instruction.
/// Used by ops / automated cleanup to reclaim rent from a stranded
/// `PollSignatures` PDA. The caller (signer, mut) receives the refund.
pub fn build_close_poll_signatures_ix(
	program_id: &Pubkey,
	caller: &Pubkey,
	req_id_pack: &[u8; 32],
	status: u8,
) -> SolanaIx {
	let (poll_sigs, _) = pda::poll_signatures_raw(program_id, req_id_pack, status);

	// Account meta order MUST match the on-chain `ClosePollSignatures`
	// struct: (caller signer+mut, poll_signatures mut).
	let accounts = vec![
		AccountMeta::new(*caller, true),    // caller (signer, mut)
		AccountMeta::new(poll_sigs, false), // poll_signatures (mut, close)
	];

	let mut data = Vec::with_capacity(8 + 32 + 1);
	data.extend_from_slice(&CLOSE_POLL_SIGNATURES_IX_DISCRIMINATOR);
	req_id_pack
		.serialize(&mut data)
		.expect("borsh serialization of req_id_pack must not fail");
	data.push(status);

	SolanaIx { program_id: *program_id, accounts, data }
}

/// Build a `poll_buffered(msg, option, asset_index)` instruction.
/// Reads signatures from the pre-populated `PollSignatures` PDA
/// instead of carrying them in instruction data.
pub fn build_poll_buffered_ix(
	program_id: &Pubkey,
	relayer: &Pubkey,
	msg: &SocketMessage,
	option: &[u8; 32],
	asset_index: &AssetIndex,
	tokens: &PollTokenAccounts,
) -> SolanaIx {
	let (socket_config, _) = pda::socket_config(program_id);
	let (vault_config, _) = pda::vault_config(program_id);
	let (prev_round, _) = pda::round_info(program_id, msg.req_id.round_id);
	let rid_pack = msg.req_id.pack();
	let (request_record, _) = pda::request_record(
		program_id,
		&pda::RequestId {
			chain: msg.req_id.chain.0,
			round_id: msg.req_id.round_id,
			sequence: msg.req_id.sequence,
		},
	);
	let (asset_config, _) = pda::asset_config(program_id, &asset_index.0);
	let (poll_sigs, _) = pda::poll_signatures_raw(program_id, &rid_pack, msg.status);

	// Same account order as Poll, plus poll_signatures at the end.
	let accounts = vec![
		AccountMeta::new(*relayer, true),                        // relayer
		AccountMeta::new(socket_config, false),                  // socket_config
		AccountMeta::new_readonly(vault_config, false),          // vault_config
		AccountMeta::new_readonly(prev_round, false),            // prev_round
		AccountMeta::new(request_record, false),                 // request_record
		AccountMeta::new_readonly(asset_config, false),          // asset_config
		AccountMeta::new_readonly(tokens.mint, false),           // mint
		AccountMeta::new(tokens.vault_token_account, false),     // vault_token_account
		AccountMeta::new(tokens.recipient_token_account, false), // recipient_token_account
		AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),  // token_program
		AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),     // system_program
		AccountMeta::new(poll_sigs, false),                      // poll_signatures (close)
	];

	let mut data = Vec::with_capacity(8 + 512);
	data.extend_from_slice(&POLL_BUFFERED_IX_DISCRIMINATOR);
	msg.serialize(&mut data)
		.expect("borsh serialization of SocketMessage must not fail");
	option
		.serialize(&mut data)
		.expect("borsh serialization of option must not fail");
	asset_index
		.serialize(&mut data)
		.expect("borsh serialization of AssetIndex must not fail");

	SolanaIx { program_id: *program_id, accounts, data }
}

/// Read the low 8 bytes of `submit.round` (uint256 BE) as a `u64`. The
/// CCCP protocol only allocates 64 bits of round id so any high bits
/// being set is a programming error in the relayer.
fn round_id_from_submit(submit: &RoundUpSubmit) -> u64 {
	let mut buf = [0u8; 8];
	buf.copy_from_slice(&submit.round[24..32]);
	u64::from_be_bytes(buf)
}

#[cfg(test)]
mod close_poll_signatures_tests {
	use super::*;
	use crate::sol::codec::CLOSE_POLL_SIGNATURES_IX_DISCRIMINATOR;
	use borsh::BorshDeserialize;

	#[test]
	fn close_poll_signatures_ix_layout() {
		let program_id = Pubkey::new_unique();
		let caller = Pubkey::new_unique();
		let rid_pack = [0xab; 32];
		let status = 10u8;

		let ix = build_close_poll_signatures_ix(&program_id, &caller, &rid_pack, status);

		// program_id matches
		assert_eq!(ix.program_id, program_id);

		// account order: [caller (signer+mut), poll_signatures (mut)]
		assert_eq!(ix.accounts.len(), 2);
		assert_eq!(ix.accounts[0].pubkey, caller);
		assert!(ix.accounts[0].is_signer);
		assert!(ix.accounts[0].is_writable);
		assert_eq!(
			ix.accounts[1].pubkey,
			pda::poll_signatures_raw(&program_id, &rid_pack, status).0
		);
		assert!(ix.accounts[1].is_writable);

		// data = discriminator || rid_pack || status
		assert_eq!(&ix.data[0..8], &CLOSE_POLL_SIGNATURES_IX_DISCRIMINATOR);
		let mut tail = &ix.data[8..];
		let decoded_rid = <[u8; 32]>::deserialize(&mut tail).expect("rid_pack decodes");
		assert_eq!(decoded_rid, rid_pack);
		assert_eq!(tail.len(), 1, "only status byte should remain");
		assert_eq!(tail[0], status);
	}

	#[test]
	fn close_poll_signatures_pda_matches_submit_signatures_pda() {
		// Regression: the cleanup IX must target the same PDA that
		// `submit_signatures` / `poll_buffered` use, otherwise `close`
		// never reaches the stranded account.
		let program_id = Pubkey::new_unique();
		let caller = Pubkey::new_unique();
		let rid_pack = [0x42; 32];
		let status = 5u8;

		let close_ix = build_close_poll_signatures_ix(&program_id, &caller, &rid_pack, status);
		let submit_ix = build_submit_signatures_ix(
			&program_id,
			&caller,
			&Signatures::default(),
			&rid_pack,
			status,
		);

		// submit_signatures puts poll_signatures at index 2; close puts
		// it at index 1. Compare by pubkey, not by position.
		let close_pda = close_ix.accounts[1].pubkey;
		let submit_pda = submit_ix.accounts[2].pubkey;
		assert_eq!(close_pda, submit_pda);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::sol::codec::{
		AssetIndex, ChainIndex, Instruction as IxCode, RBCmethod, RequestId, Signatures,
		SocketMessage, TaskParams,
	};
	use borsh::BorshDeserialize;

	fn fake_program_id() -> Pubkey {
		Pubkey::new_unique()
	}

	fn fake_token_accounts() -> PollTokenAccounts {
		PollTokenAccounts {
			mint: Pubkey::new_unique(),
			vault_token_account: Pubkey::new_unique(),
			recipient_token_account: Pubkey::new_unique(),
		}
	}

	fn sample_submit() -> PollSubmit {
		PollSubmit {
			msg: SocketMessage {
				req_id: RequestId {
					chain: ChainIndex([0x00, 0x00, 0x0b, 0xfc]),
					round_id: 1,
					sequence: 7,
				},
				status: 5,
				ins_code: IxCode {
					chain: ChainIndex(*b"SOL\0"),
					method: RBCmethod([
						0x02, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
						0x00, 0x00, 0x00, 0x00,
					]),
				},
				params: TaskParams {
					token_idx0: AssetIndex([0xab; 32]),
					token_idx1: AssetIndex::ZERO,
					refund: [0xcc; 20],
					to: [0xdd; 20],
					amount: {
						let mut a = [0u8; 32];
						a[24..32].copy_from_slice(&500_000u64.to_be_bytes());
						a
					},
					variants: vec![],
				},
			},
			sigs: Signatures::default(),
			option: [0u8; 32],
		}
	}

	#[test]
	fn poll_ix_data_starts_with_discriminator() {
		let pid = fake_program_id();
		let relayer = Pubkey::new_unique();
		let submit = sample_submit();
		let asset_index = AssetIndex([0xab; 32]);
		let ix = build_poll_ix(&pid, &relayer, &submit, &asset_index, &fake_token_accounts());

		assert_eq!(&ix.data[..8], &POLL_IX_DISCRIMINATOR);
		// The data after the discriminator must round-trip into PollSubmit.
		let mut tail = &ix.data[8..];
		let _decoded = PollSubmit::deserialize(&mut tail).expect("borsh roundtrip PollSubmit");
		// The remaining bytes must round-trip into AssetIndex.
		let _decoded_idx = AssetIndex::deserialize(&mut tail).expect("borsh roundtrip AssetIndex");
		assert!(tail.is_empty(), "no trailing bytes after asset_index");
	}

	#[test]
	fn poll_ix_account_metas_have_expected_layout() {
		let pid = fake_program_id();
		let relayer = Pubkey::new_unique();
		let submit = sample_submit();
		let tokens = fake_token_accounts();
		let ix = build_poll_ix(&pid, &relayer, &submit, &AssetIndex([0xab; 32]), &tokens);

		// 11 accounts in the order documented above.
		assert_eq!(ix.accounts.len(), 11);

		// [0] relayer — signer + writable
		assert_eq!(ix.accounts[0].pubkey, relayer);
		assert!(ix.accounts[0].is_signer);
		assert!(ix.accounts[0].is_writable);

		// [1] socket_config — writable PDA derived deterministically
		assert_eq!(ix.accounts[1].pubkey, pda::socket_config(&pid).0);
		assert!(!ix.accounts[1].is_signer);
		assert!(ix.accounts[1].is_writable);

		// [3] prev_round — keyed by msg.req_id.round_id (= 1)
		assert_eq!(ix.accounts[3].pubkey, pda::round_info(&pid, 1).0);

		// [4] request_record — writable, derived from packed rid
		let expected_record = pda::request_record(
			&pid,
			&pda::RequestId { chain: [0x00, 0x00, 0x0b, 0xfc], round_id: 1, sequence: 7 },
		)
		.0;
		assert_eq!(ix.accounts[4].pubkey, expected_record);
		assert!(ix.accounts[4].is_writable);

		// [7] vault_token_account — writable
		assert_eq!(ix.accounts[7].pubkey, tokens.vault_token_account);
		assert!(ix.accounts[7].is_writable);

		// [9] token_program — readonly, hard-coded SPL Token program ID
		assert_eq!(ix.accounts[9].pubkey, SPL_TOKEN_PROGRAM_ID);
		assert!(!ix.accounts[9].is_writable);

		// [10] system_program — readonly
		assert_eq!(ix.accounts[10].pubkey, SYSTEM_PROGRAM_ID);
	}

	#[test]
	fn token_program_id_const_matches_canonical_base58() {
		// The canonical SPL Token program ID is
		// `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`. Since we
		// hard-coded the raw bytes above, this test re-derives them from
		// the base58 form via solana-sdk and asserts equality so any
		// future copy-paste error fails loudly.
		let parsed: Pubkey = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".parse().unwrap();
		assert_eq!(parsed, SPL_TOKEN_PROGRAM_ID);
	}

	#[test]
	fn system_program_id_const_is_all_zeros() {
		let parsed: Pubkey = "11111111111111111111111111111111".parse().unwrap();
		assert_eq!(parsed, SYSTEM_PROGRAM_ID);
	}

	#[test]
	fn round_control_relay_ix_layout() {
		let pid = fake_program_id();
		let relayer = Pubkey::new_unique();

		let mut round = [0u8; 32];
		round[31] = 5; // promoting to round 5
		let submit = RoundUpSubmit {
			round,
			new_relayers: vec![[0xaa; 20], [0xbb; 20]],
			sigs: Signatures::default(),
		};
		let ix = build_round_control_relay_ix(&pid, &relayer, &submit);

		assert_eq!(ix.accounts.len(), 5);
		assert!(ix.accounts[0].is_signer);
		assert_eq!(ix.accounts[1].pubkey, pda::socket_config(&pid).0);
		assert_eq!(ix.accounts[2].pubkey, pda::round_info(&pid, 4).0); // current = 4
		assert_eq!(ix.accounts[3].pubkey, pda::round_info(&pid, 5).0); // new = 5
		assert!(ix.accounts[3].is_writable);
		assert_eq!(ix.accounts[4].pubkey, SYSTEM_PROGRAM_ID);

		assert_eq!(&ix.data[..8], &ROUND_CONTROL_RELAY_IX_DISCRIMINATOR);
		let mut tail = &ix.data[8..];
		let decoded = RoundUpSubmit::deserialize(&mut tail).expect("borsh roundtrip RoundUpSubmit");
		assert_eq!(decoded.round[31], 5);
		assert_eq!(decoded.new_relayers.len(), 2);
	}
}
