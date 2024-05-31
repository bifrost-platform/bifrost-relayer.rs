use std::{str::FromStr, sync::Arc, time::Duration};

use bitcoincore_rpc::{
	bitcoin::{hashes::sha256d::Hash, Address, Amount, Psbt, TxOut, Txid},
	bitcoincore_rpc_json::GetRawTransactionResult,
	Client as BtcClient, Error, RpcApi,
};
use br_client::eth::EthClient;
use br_primitives::{
	constants::{
		errors::{INVALID_PERIODIC_SCHEDULE, PROVIDER_INTERNAL_ERROR},
		schedule::BITCOIN_ROLLBACK_CHECK_SCHEDULE,
		tx::{DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS},
	},
	contracts::socket_queue::SocketQueueContract,
	tx::XtRequestSender,
	utils::sub_display_format,
};
use cron::Schedule;
use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{Bytes, H160, H256, U256},
};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::sleep;
use tokio_stream::StreamExt;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "rollback-verifier";

type EvmRollbackRequestOf = (
	Bytes,     // unsigned_psbt
	H160,      // who
	[u8; 32],  // txid
	U256,      // vout
	String,    // to (=vault)
	U256,      // amount
	Vec<H160>, // authorities
	Vec<bool>, // votes
	bool,      // is_approved
);

pub struct BitcoinRollbackVerifier<T> {
	/// The Bitcoin client.
	btc_client: BtcClient,
	bfc_client: Arc<EthClient<T>>,
	xt_request_sender: Arc<XtRequestSender>,
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<C: JsonRpcClient> RpcApi for BitcoinRollbackVerifier<C> {
	async fn call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> bitcoincore_rpc::Result<T> {
		let mut latest_error = Error::ReturnedError(PROVIDER_INTERNAL_ERROR.to_string());
		for _ in 0..DEFAULT_CALL_RETRIES {
			match self.btc_client.call(cmd, args).await {
				Ok(ret) => return Ok(ret),
				Err(e) => {
					latest_error = e;
				},
			}
			sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
		}
		Err(latest_error)
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient> PeriodicWorker for BitcoinRollbackVerifier<T> {
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) {
		loop {
			self.wait_until_next_time().await;

			if self.bfc_client.is_selected_relayer().await {
				let pending_rollback_psbts = self.get_rollback_psbts().await;

				log::info!(
					target: &self.bfc_client.get_chain_name(),
					"-[{}] 🔃 Pending rollback psbts: {:?}",
					sub_display_format(SUB_LOG_TARGET),
					pending_rollback_psbts.len()
				);

				let mut stream = tokio_stream::iter(pending_rollback_psbts);
				while let Some(raw_psbt) = stream.next().await {
					let psbt = Psbt::deserialize(&raw_psbt).unwrap();
					let request = self
						.get_rollback_request(
							H256::from_str(&psbt.unsigned_tx.txid().to_string()).unwrap(),
						)
						.await;

					// the request must exist
					if request.1.is_zero() {
						continue;
					}
					// the request has already been approved
					if request.8 {
						continue;
					}

					let mut is_approved = false;

					match self
						.btc_client
						.get_raw_transaction_info(
							&Txid::from_raw_hash(*Hash::from_bytes_ref(&request.2)),
							None,
						)
						.await
					{
						Ok(tx) => {
							if self.is_rollback_valid(tx, request) {
								is_approved = true;
							}
						},
						Err(_) => {
							// failed to fetch transaction
						},
					}

					// build payload
				}
			}
		}
	}
}

impl<T: JsonRpcClient> BitcoinRollbackVerifier<T> {
	pub fn new(
		btc_client: BtcClient,
		bfc_client: Arc<EthClient<T>>,
		xt_request_sender: Arc<XtRequestSender>,
	) -> Self {
		Self {
			btc_client,
			bfc_client,
			xt_request_sender,
			schedule: Schedule::from_str(BITCOIN_ROLLBACK_CHECK_SCHEDULE)
				.expect(INVALID_PERIODIC_SCHEDULE),
		}
	}

	fn is_rollback_valid(
		&self,
		tx: GetRawTransactionResult,
		request: EvmRollbackRequestOf,
	) -> bool {
		let index = request.3.as_usize();
		// output[index] must exist
		if tx.vout.len() < index {
			return false;
		}
		let output = tx.vout[index].clone();
		if let Some(to) = output.script_pub_key.address {
			// output.to must match
			if to != Address::from_str(&request.4).unwrap() {
				return false;
			}
			// output.amount must match
			if output.value != Amount::from_sat(request.5.as_u64()) {
				return false;
			}
			return true;
		}
		return false;
	}

	async fn get_rollback_psbts(&self) -> Vec<Bytes> {
		self.bfc_client
			.contract_call(self.socket_queue().rollback_psbts(), "socket_queue.rollback_psbts")
			.await
	}

	async fn get_rollback_request(&self, txid: H256) -> EvmRollbackRequestOf {
		self.bfc_client
			.contract_call(
				self.socket_queue().rollback_request(txid.into()),
				"socket_queue.rollback_request",
			)
			.await
	}

	fn socket_queue(&self) -> &SocketQueueContract<Provider<T>> {
		self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap()
	}
}
