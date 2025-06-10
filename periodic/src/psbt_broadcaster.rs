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
use miniscript::bitcoin::Psbt;
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

						match self
							.btc_client
							.get_raw_transaction(&psbt.unsigned_tx.txid(), None)
							.await
						{
							// If the transaction is already broadcasted
							Ok(tx) => {
								log::info!(
									target: &self.bfc_client.get_chain_name(),
									"-[{}] BRP-Outbound: Transaction already broadcasted {:?}",
									sub_display_format(SUB_LOG_TARGET),
									tx.txid(),
								);
							},
							// If the transaction is not broadcasted
							Err(_) => {
								// Try to broadcast the transaction
								match self
									.btc_client
									.send_raw_transaction(
										&psbt.clone().extract_tx().expect("fee rate too high"),
									)
									.await
								{
									Ok(txid) => {
										log::info!(
											target: &self.bfc_client.get_chain_name(),
											"-[{}] BRP-Outbound: Broadcasted {:?}",
											sub_display_format(SUB_LOG_TARGET),
											txid,
										);
									},
									Err(e) => {
										log::warn!(
											target: &self.bfc_client.get_chain_name(),
											"-[{}] BRP-Outbound: Broadcast failed {:?}",
											sub_display_format(SUB_LOG_TARGET),
											e
										);
									},
								}
							},
						}
					}
				}
			}
		}
	}
}
