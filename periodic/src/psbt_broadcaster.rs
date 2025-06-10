use std::{str::FromStr, sync::Arc};

use alloy::{
	network::AnyNetwork,
	primitives::Bytes,
	providers::{Provider, WalletProvider, fillers::TxFiller},
};
use bitcoincore_rpc::{Client as BtcClient, RpcApi};
use br_client::eth::EthClient;
use br_primitives::{
	constants::{errors::INVALID_PERIODIC_SCHEDULE, schedule::PSBT_BROADCASTER_SCHEDULE},
	utils::sub_display_format,
};
use cron::Schedule;
use eyre::Result;
use miniscript::bitcoin::{Psbt, Txid};
use tokio_stream::StreamExt;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "psbt-broadcaster";

/// The essential task that broadcast finalized PSBT's.
pub struct PsbtBroadcaster<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// The Bifrost client.
	pub bfc_client: Arc<EthClient<F, P>>,
	/// The Bitcoin client.
	btc_client: BtcClient,
	/// Loop schedule.
	schedule: Schedule,
}

impl<F, P> PsbtBroadcaster<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	pub fn new(bfc_client: Arc<EthClient<F, P>>, btc_client: BtcClient) -> Self {
		Self {
			bfc_client,
			btc_client,
			schedule: Schedule::from_str(PSBT_BROADCASTER_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
		}
	}

	/// Get the finalized PSBT's (in bytes)
	async fn get_finalized_psbts(&self) -> Result<Vec<Bytes>> {
		let socket_queue = self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap();
		let res = socket_queue.finalized_psbts().call().await?._0;
		Ok(res)
	}

	/// Check if the transaction is already broadcasted
	async fn is_transaction_broadcasted(&self, txid: Txid) -> Result<bool> {
		let tx = self.btc_client.get_raw_transaction(&txid, None).await;
		Ok(tx.is_ok())
	}

	/// Try to broadcast the transaction
	async fn broadcast_transaction(&self, psbt: Psbt) {
		let tx = psbt.clone().extract_tx().expect("fee rate too high");

		// First test if the transaction would be accepted
		match self.btc_client.test_mempool_accept(&[&tx]).await {
			Ok(results) => {
				if let Some(result) = results.first() {
					if result.allowed {
						log::info!(
							target: &self.bfc_client.get_chain_name(),
							"-[{}] BRP-Outbound: Transaction passed mempool test, broadcasting...",
							sub_display_format(SUB_LOG_TARGET),
						);
						match self.btc_client.send_raw_transaction(&tx).await {
							Ok(txid) => {
								log::info!(
									target: &self.bfc_client.get_chain_name(),
									"-[{}] BRP-Outbound: Broadcasted {:?}",
									sub_display_format(SUB_LOG_TARGET),
									txid,
								);
							},
							Err(e) => {
								log::error!(
									target: &self.bfc_client.get_chain_name(),
									"-[{}] BRP-Outbound: Broadcast failed: {:?}",
									sub_display_format(SUB_LOG_TARGET),
									e
								);
							},
						}
					} else {
						log::error!(
							target: &self.bfc_client.get_chain_name(),
							"-[{}] BRP-Outbound: Transaction rejected by mempool test: {}",
							sub_display_format(SUB_LOG_TARGET),
							result.reject_reason.as_ref().unwrap_or(&"unknown reason".to_string())
						);
					}
				}
			},
			Err(e) => {
				log::error!(
					target: &self.bfc_client.get_chain_name(),
					"-[{}] BRP-Outbound: Mempool test request failed: {:?}",
					sub_display_format(SUB_LOG_TARGET),
					e
				);
			},
		}
	}
}

#[async_trait::async_trait]
impl<F, P> PeriodicWorker for PsbtBroadcaster<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		loop {
			self.wait_until_next_time().await;

			if self.bfc_client.is_selected_relayer().await? {
				let finalized_psbts = self.get_finalized_psbts().await?;
				if !finalized_psbts.is_empty() {
					log::info!(
						target: &self.bfc_client.get_chain_name(),
						"-[{}] 🔐 {} finalized PSBT exists.",
						sub_display_format(SUB_LOG_TARGET),
						finalized_psbts.len()
					);

					let mut stream = tokio_stream::iter(finalized_psbts);
					while let Some(finalized_psbt) = stream.next().await {
						let psbt = Psbt::deserialize(&finalized_psbt).unwrap();

						let txid = psbt.unsigned_tx.txid();
						if self.is_transaction_broadcasted(txid).await? {
							log::info!(
								target: &self.bfc_client.get_chain_name(),
								"-[{}] BRP-Outbound: Transaction already broadcasted {:?}",
								sub_display_format(SUB_LOG_TARGET),
								txid,
							);
							continue;
						}

						self.broadcast_transaction(psbt).await;
					}
				}
			}
		}
	}
}
