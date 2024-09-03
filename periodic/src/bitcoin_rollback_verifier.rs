use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use crate::traits::PeriodicWorker;
use bitcoincore_rpc::{
	bitcoin::{hashes::sha256d::Hash, Address, Amount, Psbt, Txid},
	bitcoincore_rpc_json::GetRawTransactionResult,
	Client as BtcClient, Error, RpcApi,
};
use br_client::{btc::handlers::XtRequester, eth::EthClient};
use br_primitives::{
	constants::{
		errors::{INVALID_PERIODIC_SCHEDULE, PROVIDER_INTERNAL_ERROR},
		schedule::BITCOIN_ROLLBACK_CHECK_SCHEDULE,
		tx::{DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS},
	},
	contracts::socket_queue::SocketQueueContract,
	substrate::CustomConfig,
	substrate::{bifrost_runtime, RollbackPollMessage},
	tx::{SubmitRollbackPollMetadata, XtRequest, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};
use cron::Schedule;
use ethers::{
	providers::{JsonRpcClient, Provider},
	types::{Bytes, H160, H256, U256},
};
use serde::Deserialize;
use serde_json::Value;
use subxt::tx::Signer;
use tokio::time::sleep;
use tokio_stream::StreamExt;

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

#[derive(Debug)]
/// (Bitcoin) Rollback request information.
pub struct RollbackRequest {
	/// The unsigned PSBT of the rollback request (in bytes).
	pub unsigned_psbt: Bytes,
	/// The registered user who attemps to request.
	pub who: H160,
	/// The Bitcoin transaction hash that contains the output.
	pub txid: Txid,
	/// The output index.
	pub vout: usize,
	/// The output destination address.
	pub to: Address,
	/// The output amount.
	pub amount: Amount,
	/// The current voting state of the request.
	pub votes: BTreeMap<H160, bool>,
	/// The current voting result of the request.
	pub is_approved: bool,
}

impl From<EvmRollbackRequestOf> for RollbackRequest {
	fn from(value: EvmRollbackRequestOf) -> Self {
		let mut votes = BTreeMap::default();
		for idx in 0..value.6.len() {
			votes.insert(value.6[idx], value.7[idx]);
		}
		let mut txid = value.2;
		txid.reverse();
		Self {
			unsigned_psbt: value.0,
			who: value.1,
			txid: Txid::from_raw_hash(*Hash::from_bytes_ref(&txid)),
			vout: value.3.as_usize(),
			to: Address::from_str(&value.4).unwrap().assume_checked(),
			amount: Amount::from_sat(value.5.as_u64()),
			votes,
			is_approved: value.8,
		}
	}
}

pub struct BitcoinRollbackVerifier<T, S> {
	/// The Bitcoin client.
	btc_client: BtcClient,
	/// The Bifrost client.
	bfc_client: Arc<EthClient<T, S>>,
	/// The extrinsic message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The periodic schedule.
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<C: JsonRpcClient, S: Signer<CustomConfig> + Send + Sync> RpcApi
	for BitcoinRollbackVerifier<C, S>
{
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
impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig> + 'static + Send + Sync> XtRequester<T, S>
	for BitcoinRollbackVerifier<T, S>
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<T, S>> {
		self.bfc_client.clone()
	}
}

#[async_trait::async_trait]
impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig> + 'static + Send + Sync> PeriodicWorker
	for BitcoinRollbackVerifier<T, S>
{
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
					let txid = H256::from_str(&psbt.unsigned_tx.txid().to_string()).unwrap();
					let request = self.get_rollback_request(txid).await;

					// the request must exist
					if request.who.is_zero() {
						continue;
					}
					// the request has already been approved
					if request.is_approved {
						continue;
					}

					let mut is_approved = false;

					match self.btc_client.get_raw_transaction_info(&request.txid, None).await {
						Ok(tx) => {
							if self.is_rollback_valid(tx, &request) {
								is_approved = true;
							}
						},
						Err(_) => {
							// failed to fetch transaction
							log::warn!(
								target: &self.bfc_client.get_chain_name(),
								"-[{}] ⚠️  Failed to fetch rollback transaction: {}",
								sub_display_format(SUB_LOG_TARGET),
								&txid
							);
						},
					}

					// check if already submitted
					if let Some(vote) = request.votes.get(&self.bfc_client.address()) {
						if *vote == is_approved {
							continue;
						}
					}

					// build payload
					let (call, metadata) = self.build_extrinsic(txid, is_approved);
					self.request_send_transaction(
						call,
						XtRequestMetadata::SubmitRollbackPoll(metadata),
						SUB_LOG_TARGET,
					);
				}
			}
		}
	}
}

impl<T: JsonRpcClient + 'static, S: Signer<CustomConfig> + 'static + Send + Sync>
	BitcoinRollbackVerifier<T, S>
{
	/// Instantiates a new `BitcoinRollbackVerifier` instance.
	pub fn new(
		btc_client: BtcClient,
		bfc_client: Arc<EthClient<T, S>>,
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

	/// Verifies whether the pending request information matches with on-chain data.
	fn is_rollback_valid(&self, tx: GetRawTransactionResult, request: &RollbackRequest) -> bool {
		// output[index] must exist
		if tx.vout.len() < request.vout {
			return false;
		}
		let output = tx.vout[request.vout].clone();
		if let Some(to) = output.script_pub_key.address {
			// output.to must match
			if to != request.to {
				return false;
			}
			// output.amount must match
			if output.value != request.amount {
				return false;
			}
			return true;
		}
		return false;
	}

	/// Build the calldata for the extrinsic. (`submit_rollback_poll()`)
	fn build_extrinsic(
		&self,
		txid: H256,
		is_approved: bool,
	) -> (XtRequest, SubmitRollbackPollMetadata) {
		let msg = RollbackPollMessage { txid, is_approved };
		let metadata = SubmitRollbackPollMetadata::new(txid, is_approved);
		(
			XtRequest::from(bifrost_runtime::tx().btc_socket_queue().submit_rollback_poll(msg)),
			metadata,
		)
	}

	/// Get the pending rollback PSBT's.
	async fn get_rollback_psbts(&self) -> Vec<Bytes> {
		self.bfc_client
			.contract_call(self.socket_queue().rollback_psbts(), "socket_queue.rollback_psbts")
			.await
	}

	/// Get the rollback information.
	async fn get_rollback_request(&self, txid: H256) -> RollbackRequest {
		self.bfc_client
			.contract_call(
				self.socket_queue().rollback_request(txid.into()),
				"socket_queue.rollback_request",
			)
			.await
			.into()
	}

	/// Get the `BtcSocketQueue` precompile contract instance.
	fn socket_queue(&self) -> &SocketQueueContract<Provider<T>> {
		self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap()
	}
}
