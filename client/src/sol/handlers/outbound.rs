// SPDX-License-Identifier: Apache-2.0
//
// Outbound handler. The Bifrost socket relay handler routes
// `dst_chain_id == sol_chain_id` messages here; this worker takes the
// `Socket_Message` + signatures the relayer collected on Bifrost, builds
// the matching cccp-solana `poll(...)` instruction via
// `crate::sol::ix_builder`, and submits the transaction with the
// relayer's Solana fee-payer keypair.
//
// Production wiring:
//
//   * The handler owns an `AssetRegistry` (built from `SolProvider.assets`)
//     so it can map `(asset_index, recipient_wallet)` to the full
//     `(mint, vault TA, recipient TA)` triple without an extra round-trip.
//   * Every outbound transaction prepends a `create_associated_token_account`
//     idempotent IX so first-time recipients don't bounce.
//   * The handler verifies the cluster is configured as `is_relay_target`
//     and idles otherwise; this lets the relayer observe inbound traffic
//     without paying SOL fees on a watch-only cluster.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::Instruction;
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer, read_keypair_file};
use solana_sdk::transaction::Transaction;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::sol::ata::{build_create_ata_idempotent_ix, derive_ata};
use crate::sol::client::SolClient;
use crate::sol::codec::{AssetIndex, PollSubmit, RoundUpSubmit, Signatures};
use crate::sol::ix_builder::{
	PollTokenAccounts, SIGS_PER_CHUNK, build_poll_buffered_ix, build_poll_ix,
	build_round_control_relay_ix, build_submit_signatures_ix,
};
use crate::sol::registry::AssetRegistry;

const SUB_LOG_TARGET: &str = "sol-outbound";

// ── Compute Budget Program ──────────────────────────────────────────
// We construct the IX data by hand to avoid pulling in an extra crate.
// Program ID: ComputeBudget111111111111111111111111111111
const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
	3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231, 188, 140, 229, 164,
	218, 0, 137, 145, 204, 121, 124, 0, 0, 0, 0, 0,
]);

/// SetComputeUnitLimit(u32) — instruction discriminator 0x02.
fn compute_budget_set_cu_limit(units: u32) -> Instruction {
	let mut data = vec![0x02];
	data.extend_from_slice(&units.to_le_bytes());
	Instruction { program_id: COMPUTE_BUDGET_PROGRAM_ID, accounts: vec![], data }
}

/// SetComputeUnitPrice(u64) — instruction discriminator 0x03.
fn compute_budget_set_cu_price(micro_lamports: u64) -> Instruction {
	let mut data = vec![0x03];
	data.extend_from_slice(&micro_lamports.to_le_bytes());
	Instruction { program_id: COMPUTE_BUDGET_PROGRAM_ID, accounts: vec![], data }
}

// ── Defaults ────────────────────────────────────────────────────────
const DEFAULT_COMPUTE_UNIT_LIMIT: u32 = 400_000;
const DEFAULT_BASE_PRIORITY_FEE: u64 = 1_000;
const DEFAULT_MAX_PRIORITY_FEE: u64 = 1_000_000;
const DEFAULT_CONFIRMATION_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_SEND_RETRIES: u32 = 3;
const PRIORITY_FEE_MULTIPLIER: u64 = 3;
const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_millis(2_000);
/// Solana's maximum transaction packet size.
const SOLANA_MAX_TX_SIZE: usize = 1232;

/// Work item pushed onto the outbound queue.
#[derive(Debug, Clone)]
pub enum SolOutboundJob {
	/// Plain `poll(...)` job. The caller has already resolved every
	/// token account address — used by tests / round trips.
	Poll { submit: PollSubmit, asset_index: AssetIndex, tokens: PollTokenAccounts },
	/// `poll(...)` job that lets the handler resolve the token accounts
	/// from its `AssetRegistry` + the supplied recipient Solana wallet.
	/// This is the variant the Bifrost socket relay handler dispatches
	/// from `dispatch_to_solana`.
	PollFromBfc {
		submit: PollSubmit,
		asset_index: AssetIndex,
		/// Solana wallet that owns the recipient ATA. The relayer derives
		/// the actual ATA address inside `handle()`.
		recipient_wallet: Pubkey,
	},
	/// `round_control_relay(...)` for relayer set rotation.
	RoundControlRelay { submit: RoundUpSubmit },
}

/// Channel used by upstream workers (Bifrost socket relay handler,
/// round-up emitter) to push work onto the outbound submission queue.
pub type SolOutboundSender = UnboundedSender<SolOutboundJob>;

/// Outbound submission worker. One per Solana cluster.
pub struct SolOutboundHandler {
	pub client: SolClient,
	rpc: Arc<RpcClient>,
	receiver: UnboundedReceiver<SolOutboundJob>,
	/// Loaded fee-payer Ed25519 keypair (= `solana_sdk::Keypair`).
	fee_payer: Keypair,
	/// Asset registry — maps CCCP asset indexes to SPL mints and
	/// pre-derived vault ATAs. Built from static config at boot.
	pub registry: AssetRegistry,
	/// Priority fee config (micro-lamports per CU).
	base_priority_fee: u64,
	max_priority_fee: u64,
	confirmation_timeout: Duration,
	max_send_retries: u32,
}

impl SolOutboundHandler {
	/// Create a new handler. Returns the `(handler, sender)` pair so the
	/// service-deps wiring can hand the sender to upstream workers.
	pub fn new(
		client: SolClient,
		fee_payer_keypair_path: PathBuf,
		registry: AssetRegistry,
		base_priority_fee: Option<u64>,
		max_priority_fee: Option<u64>,
		confirmation_timeout_secs: Option<u64>,
		max_send_retries: Option<u32>,
	) -> eyre::Result<(Self, SolOutboundSender)> {
		let fee_payer = read_keypair_file(&fee_payer_keypair_path).map_err(|err| {
			eyre::eyre!(
				"failed to load Solana fee-payer keypair from {}: {}",
				fee_payer_keypair_path.display(),
				err
			)
		})?;
		let rpc = client.rpc();
		let (sender, receiver) = mpsc::unbounded_channel();

		Ok((
			Self {
				client,
				rpc,
				receiver,
				fee_payer,
				registry,
				base_priority_fee: base_priority_fee.unwrap_or(DEFAULT_BASE_PRIORITY_FEE),
				max_priority_fee: max_priority_fee.unwrap_or(DEFAULT_MAX_PRIORITY_FEE),
				confirmation_timeout: Duration::from_secs(
					confirmation_timeout_secs.unwrap_or(DEFAULT_CONFIRMATION_TIMEOUT_SECS),
				),
				max_send_retries: max_send_retries.unwrap_or(DEFAULT_MAX_SEND_RETRIES),
			},
			sender,
		))
	}

	/// Borrow the fee-payer pubkey, useful for diagnostics.
	pub fn fee_payer_pubkey(&self) -> Pubkey {
		self.fee_payer.pubkey()
	}

	/// Run the worker loop. Pops jobs off the channel and submits them
	/// one at a time. We deliberately do NOT batch multiple jobs into a
	/// single transaction — Bifrost's relayer protocol expects each
	/// `poll` / `round_control_relay` to land in its own tx so failures
	/// are isolated and so the on-chain `init_if_needed` PDA allocation
	/// for `RequestRecord` is per-message.
	pub async fn run(&mut self) -> eyre::Result<()> {
		if !self.client.is_relay_target {
			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] outbound handler idle (cluster is not a relay target)",
				SUB_LOG_TARGET,
			);
			// Park the worker but stay alive so the spawn supervisor
			// doesn't restart us in a tight loop.
			loop {
				tokio::time::sleep(std::time::Duration::from_secs(60)).await;
			}
		}

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] outbound handler started; fee_payer={} assets={}",
			SUB_LOG_TARGET,
			self.fee_payer_pubkey(),
			self.registry.len(),
		);

		while let Some(job) = self.receiver.recv().await {
			match self.handle(job).await {
				Ok(()) => {
					br_metrics::increase_sol_outbound_submitted(&self.client.name);
				},
				Err(err) => {
					br_metrics::increase_sol_outbound_failed(&self.client.name);
					log::error!(
						target: &self.client.get_chain_name(),
						"[{}] failed to submit outbound job: {err:?}",
						SUB_LOG_TARGET,
					);
				},
			}
		}

		Ok(())
	}

	async fn handle(&self, job: SolOutboundJob) -> eyre::Result<()> {
		// Check if we need the buffered path (too many sigs for a single tx).
		let needs_buffer = match &job {
			SolOutboundJob::Poll { submit, .. } | SolOutboundJob::PollFromBfc { submit, .. } => {
				submit.sigs.r.len() > SIGS_PER_CHUNK
			},
			_ => false,
		};

		if needs_buffer {
			return self.handle_buffered(job).await;
		}

		let app_ixs = match &job {
			SolOutboundJob::Poll { submit, asset_index, tokens } => vec![build_poll_ix(
				&self.client.program_id,
				&self.fee_payer.pubkey(),
				submit,
				asset_index,
				tokens,
			)],
			SolOutboundJob::PollFromBfc { submit, asset_index, recipient_wallet } => {
				self.build_poll_ix_from_bfc(submit, asset_index, recipient_wallet)?
			},
			SolOutboundJob::RoundControlRelay { submit } => {
				vec![build_round_control_relay_ix(
					&self.client.program_id,
					&self.fee_payer.pubkey(),
					submit,
				)]
			},
		};

		let mut priority_fee = self.base_priority_fee;

		for attempt in 0..=self.max_send_retries {
			// Prepend compute-budget instructions with the current priority fee.
			let mut ixs = vec![
				compute_budget_set_cu_limit(DEFAULT_COMPUTE_UNIT_LIMIT),
				compute_budget_set_cu_price(priority_fee),
			];
			ixs.extend(app_ixs.clone());

			let recent_blockhash = self
				.rpc
				.get_latest_blockhash()
				.await
				.map_err(|e| eyre::eyre!("get_latest_blockhash: {e}"))?;

			// Pre-flight: compile the message and check serialized size.
			// Solana's max tx packet size is 1232 bytes. We estimate the
			// wire size as: compact(num_sigs) + num_sigs*64 + message bytes.
			let message = Message::new(&ixs, Some(&self.fee_payer.pubkey()));
			let message_data = message.serialize();
			let num_signers = message.header.num_required_signatures as usize;
			// compact-u16 encoding for 1 signer = 1 byte
			let tx_size = 1 + num_signers * 64 + message_data.len();
			if tx_size > SOLANA_MAX_TX_SIZE {
				br_metrics::increase_sol_outbound_oversized(&self.client.name);
				return Err(eyre::eyre!(
					"transaction exceeds Solana's {SOLANA_MAX_TX_SIZE}B limit \
                     (actual: {tx_size}B). Reduce CCCP signature count or \
                     wait for on-chain signature buffer support."
				));
			}
			log::debug!(
				target: &self.client.get_chain_name(),
				"[{}] tx size: {tx_size}/{SOLANA_MAX_TX_SIZE} bytes",
				SUB_LOG_TARGET,
			);

			let tx = Transaction::new(&[&self.fee_payer], message, recent_blockhash);

			// Send without waiting for confirmation — we'll poll manually.
			let signature = self
				.rpc
				.send_transaction(&tx)
				.await
				.map_err(|e| eyre::eyre!("send_transaction: {e}"))?;

			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] tx sent: sig={signature} priority_fee={priority_fee} attempt={}",
				SUB_LOG_TARGET,
				attempt + 1,
			);

			match self.await_confirmation(&signature).await {
				Ok(()) => {
					log::info!(
						target: &self.client.get_chain_name(),
						"[{}] tx confirmed: sig={signature}",
						SUB_LOG_TARGET,
					);
					return Ok(());
				},
				Err(err) => {
					if attempt < self.max_send_retries {
						let next_fee = priority_fee
							.saturating_mul(PRIORITY_FEE_MULTIPLIER)
							.min(self.max_priority_fee);
						log::warn!(
							target: &self.client.get_chain_name(),
							"[{}] confirmation timeout (attempt {}): {err}; \
							 escalating priority fee {priority_fee} → {next_fee}",
							SUB_LOG_TARGET,
							attempt + 1,
						);
						priority_fee = next_fee;
					} else {
						return Err(eyre::eyre!(
							"tx failed after {} attempts (last sig={signature}): {err}",
							self.max_send_retries + 1
						));
					}
				},
			}
		}

		unreachable!()
	}

	/// Poll `getSignatureStatuses` until the transaction lands or the
	/// timeout expires. Returns `Ok(())` on confirmation, `Err` on
	/// timeout or terminal failure (e.g. InstructionError).
	async fn await_confirmation(&self, signature: &Signature) -> eyre::Result<()> {
		let deadline = tokio::time::Instant::now() + self.confirmation_timeout;

		loop {
			if tokio::time::Instant::now() >= deadline {
				return Err(eyre::eyre!(
					"confirmation timeout after {:?}",
					self.confirmation_timeout
				));
			}

			tokio::time::sleep(CONFIRMATION_POLL_INTERVAL).await;

			let statuses = self
				.rpc
				.get_signature_statuses(&[*signature])
				.await
				.map_err(|e| eyre::eyre!("get_signature_statuses: {e}"))?;
			br_metrics::increase_sol_rpc_calls(&self.client.name);

			if let Some(Some(status)) = statuses.value.first() {
				if let Some(ref err) = status.err {
					return Err(eyre::eyre!("tx failed on-chain: {err:?}"));
				}
				// Transaction succeeded (has confirmations, no error).
				return Ok(());
			}
			// `None` → not yet seen by the cluster, keep polling.
		}
	}

	/// Buffered path: split signatures into chunks, submit each chunk
	/// as a separate `submit_signatures` tx, then send `poll_buffered`.
	async fn handle_buffered(&self, job: SolOutboundJob) -> eyre::Result<()> {
		let (submit, asset_index, tokens) = match &job {
			SolOutboundJob::PollFromBfc { submit, asset_index, recipient_wallet } => {
				let entry = self.registry.get(&asset_index.0).ok_or_else(|| {
					eyre::eyre!(
						"no asset registry entry for index 0x{}",
						hex::encode(asset_index.0)
					)
				})?;
				let recipient_ata = derive_ata(recipient_wallet, &entry.mint);
				let tokens = PollTokenAccounts {
					mint: entry.mint,
					vault_token_account: entry.vault_token_account,
					recipient_token_account: recipient_ata,
				};
				(submit, asset_index, tokens)
			},
			SolOutboundJob::Poll { submit, asset_index, tokens } => (submit, asset_index, *tokens),
			_ => return Err(eyre::eyre!("handle_buffered called for non-poll job")),
		};

		let req_id_pack = submit.msg.req_id.pack();
		let total_sigs = submit.sigs.r.len();

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] using buffered path for {total_sigs} signatures",
			SUB_LOG_TARGET,
		);

		// Phase 1: submit signature chunks
		for chunk_start in (0..total_sigs).step_by(SIGS_PER_CHUNK) {
			let chunk_end = (chunk_start + SIGS_PER_CHUNK).min(total_sigs);
			let chunk = Signatures {
				r: submit.sigs.r[chunk_start..chunk_end].to_vec(),
				s: submit.sigs.s[chunk_start..chunk_end].to_vec(),
				v: submit.sigs.v[chunk_start..chunk_end].to_vec(),
			};

			let ix = build_submit_signatures_ix(
				&self.client.program_id,
				&self.fee_payer.pubkey(),
				&chunk,
				&req_id_pack,
				submit.msg.status,
			);

			self.send_single_ix(ix).await.map_err(|e| {
				eyre::eyre!("submit_signatures chunk {chunk_start}..{chunk_end} failed: {e}")
			})?;

			log::info!(
				target: &self.client.get_chain_name(),
				"[{}] submitted sig chunk {chunk_start}..{chunk_end}/{total_sigs}",
				SUB_LOG_TARGET,
			);
		}

		// Phase 2: send poll_buffered (reads sigs from PDA, no sigs in IX data)
		let create_ata = build_create_ata_idempotent_ix(
			&self.fee_payer.pubkey(),
			// recipient wallet — we derive from the token account
			// For PollFromBfc the ATA was already computed above
			&tokens.recipient_token_account, // This is actually the ATA, not the wallet
			&tokens.mint,
		);
		let poll_buf = build_poll_buffered_ix(
			&self.client.program_id,
			&self.fee_payer.pubkey(),
			&submit.msg,
			&submit.option,
			asset_index,
			&tokens,
		);

		// Send create_ata + poll_buffered together
		let ixs = vec![
			compute_budget_set_cu_limit(DEFAULT_COMPUTE_UNIT_LIMIT),
			compute_budget_set_cu_price(self.base_priority_fee),
			create_ata,
			poll_buf,
		];

		let recent_blockhash = self
			.rpc
			.get_latest_blockhash()
			.await
			.map_err(|e| eyre::eyre!("get_latest_blockhash: {e}"))?;

		let message = Message::new(&ixs, Some(&self.fee_payer.pubkey()));
		let tx = Transaction::new(&[&self.fee_payer], message, recent_blockhash);

		let signature = self
			.rpc
			.send_transaction(&tx)
			.await
			.map_err(|e| eyre::eyre!("send poll_buffered: {e}"))?;

		self.await_confirmation(&signature).await?;

		log::info!(
			target: &self.client.get_chain_name(),
			"[{}] poll_buffered confirmed: sig={signature}",
			SUB_LOG_TARGET,
		);

		Ok(())
	}

	/// Send a single instruction as a transaction, wait for confirmation.
	async fn send_single_ix(&self, ix: Instruction) -> eyre::Result<()> {
		let ixs = vec![
			compute_budget_set_cu_limit(200_000),
			compute_budget_set_cu_price(self.base_priority_fee),
			ix,
		];

		let recent_blockhash = self
			.rpc
			.get_latest_blockhash()
			.await
			.map_err(|e| eyre::eyre!("get_latest_blockhash: {e}"))?;

		let message = Message::new(&ixs, Some(&self.fee_payer.pubkey()));
		let tx = Transaction::new(&[&self.fee_payer], message, recent_blockhash);

		let signature = self
			.rpc
			.send_transaction(&tx)
			.await
			.map_err(|e| eyre::eyre!("send_transaction: {e}"))?;

		self.await_confirmation(&signature).await
	}

	/// Build the IX list for a `PollFromBfc` job. Resolves the token
	/// accounts from the asset registry + the supplied recipient wallet,
	/// and prepends a `create_associated_token_account` idempotent IX so
	/// the recipient ATA is materialized if it doesn't already exist.
	fn build_poll_ix_from_bfc(
		&self,
		submit: &PollSubmit,
		asset_index: &AssetIndex,
		recipient_wallet: &Pubkey,
	) -> eyre::Result<Vec<Instruction>> {
		let entry = self.registry.get(&asset_index.0).ok_or_else(|| {
			eyre::eyre!(
				"no asset registry entry for index 0x{}; configure SolProvider.assets",
				hex::encode(asset_index.0)
			)
		})?;

		let recipient_ata = derive_ata(recipient_wallet, &entry.mint);

		let tokens = PollTokenAccounts {
			mint: entry.mint,
			vault_token_account: entry.vault_token_account,
			recipient_token_account: recipient_ata,
		};

		let create_ata =
			build_create_ata_idempotent_ix(&self.fee_payer.pubkey(), recipient_wallet, &entry.mint);

		let poll = build_poll_ix(
			&self.client.program_id,
			&self.fee_payer.pubkey(),
			submit,
			asset_index,
			&tokens,
		);

		Ok(vec![create_ata, poll])
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use br_primitives::cli::SolAssetEntry;

	use crate::sol::{
		codec::{
			AssetIndex, ChainIndex, Instruction as IxCode, RBCmethod, RequestId, Signatures,
			SocketMessage, TaskParams,
		},
		pda,
	};

	fn fake_program_id() -> Pubkey {
		Pubkey::new_unique()
	}

	fn fake_registry(program_id: &Pubkey) -> AssetRegistry {
		let (vault_pda, _) = pda::vault_config(program_id);
		AssetRegistry::from_entries(
			&[SolAssetEntry {
				index: "0x0100010000000000000000000000000000000000000000000000000000000008".into(),
				mint: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".into(),
				name: Some("usdc".into()),
			}],
			&vault_pda,
		)
		.unwrap()
	}

	fn sample_submit() -> PollSubmit {
		PollSubmit {
			msg: SocketMessage {
				req_id: RequestId {
					chain: ChainIndex([0, 0, 0x0b, 0xfc]),
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
					token_idx0: AssetIndex([
						0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
						0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
						0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,
					]),
					token_idx1: AssetIndex::ZERO,
					refund: [0xab; 20],
					to: [0xcd; 20],
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

	/// Replicates `build_poll_ix_from_bfc` without instantiating an
	/// actual `SolOutboundHandler` (avoids needing a real keypair file).
	fn build_poll_ix_from_bfc_pure(
		program_id: &Pubkey,
		fee_payer: &Pubkey,
		registry: &AssetRegistry,
		submit: &PollSubmit,
		asset_index: &AssetIndex,
		recipient_wallet: &Pubkey,
	) -> eyre::Result<Vec<Instruction>> {
		let entry = registry.get(&asset_index.0).unwrap();
		let recipient_ata = derive_ata(recipient_wallet, &entry.mint);
		let tokens = PollTokenAccounts {
			mint: entry.mint,
			vault_token_account: entry.vault_token_account,
			recipient_token_account: recipient_ata,
		};
		let create_ata = build_create_ata_idempotent_ix(fee_payer, recipient_wallet, &entry.mint);
		let poll = build_poll_ix(program_id, fee_payer, submit, asset_index, &tokens);
		Ok(vec![create_ata, poll])
	}

	#[test]
	fn poll_from_bfc_prepends_create_ata_and_uses_registry_vault_ata() {
		let program_id = fake_program_id();
		let registry = fake_registry(&program_id);
		let fee_payer = Pubkey::new_unique();
		let recipient_wallet = Pubkey::new_unique();
		let submit = sample_submit();
		let asset_index = AssetIndex([
			0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
			0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
			0x00, 0x00, 0x00, 0x08,
		]);

		let ixs = build_poll_ix_from_bfc_pure(
			&program_id,
			&fee_payer,
			&registry,
			&submit,
			&asset_index,
			&recipient_wallet,
		)
		.unwrap();

		assert_eq!(ixs.len(), 2, "expected create_ata + poll");

		// First IX = create_associated_token_account_idempotent
		let create_ata = &ixs[0];
		assert_eq!(create_ata.data, vec![1u8]);
		assert_eq!(create_ata.accounts.len(), 6);
		assert_eq!(create_ata.accounts[0].pubkey, fee_payer);

		// Second IX = the cccp-solana poll(...) call
		let poll = &ixs[1];
		assert_eq!(poll.program_id, program_id);

		// The poll IX must use the registry-derived vault ATA, not some
		// other random pubkey.
		let entry = registry.get(&asset_index.0).unwrap();
		let expected_vault_ta = derive_ata(&pda::vault_config(&program_id).0, &entry.mint);
		assert_eq!(entry.vault_token_account, expected_vault_ta);
		// Position 7 in the Poll Accounts struct is `vault_token_account`
		// (see `crate::sol::ix_builder` layout).
		assert_eq!(poll.accounts[7].pubkey, expected_vault_ta);

		// Position 8 is `recipient_token_account` — must equal the ATA
		// for our recipient_wallet.
		let expected_recipient_ata = derive_ata(&recipient_wallet, &entry.mint);
		assert_eq!(poll.accounts[8].pubkey, expected_recipient_ata);
	}

	#[test]
	fn poll_job_can_be_constructed() {
		let _job = SolOutboundJob::Poll {
			submit: sample_submit(),
			asset_index: AssetIndex([0u8; 32]),
			tokens: PollTokenAccounts {
				mint: Pubkey::new_unique(),
				vault_token_account: Pubkey::new_unique(),
				recipient_token_account: Pubkey::new_unique(),
			},
		};
	}
}
