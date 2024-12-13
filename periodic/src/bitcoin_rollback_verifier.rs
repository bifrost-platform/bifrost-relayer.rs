use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use alloy::{
	network::AnyNetwork,
	primitives::{keccak256, Address as EvmAddress, Bytes, B256},
	providers::{
		fillers::{FillProvider, TxFiller},
		Provider, WalletProvider,
	},
	transports::Transport,
};
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
	contracts::socket_queue::SocketQueueContract::{
		rollback_requestReturn, SocketQueueContractInstance,
	},
	substrate::{bifrost_runtime, AccountId20, EthereumSignature, RollbackPollMessage},
	tx::{SubmitRollbackPollMetadata, XtRequest, XtRequestMetadata, XtRequestSender},
	utils::sub_display_format,
};
use cron::Schedule;
use eyre::Result;
use serde::Deserialize;
use serde_json::Value;
use subxt::utils::H256;
use tokio::time::sleep;
use tokio_stream::StreamExt;

use crate::traits::PeriodicWorker;

const SUB_LOG_TARGET: &str = "rollback-verifier";

#[derive(Debug)]
/// (Bitcoin) Rollback request information.
pub struct RollbackRequest {
	/// The unsigned PSBT of the rollback request (in bytes).
	pub unsigned_psbt: Bytes,
	/// The registered user who attempts to request.
	pub who: EvmAddress,
	/// The Bitcoin transaction hash that contains the output.
	pub txid: Txid,
	/// The output index.
	pub vout: usize,
	/// The output destination address.
	pub to: Address,
	/// The output amount.
	pub amount: Amount,
	/// The current voting state of the request.
	pub votes: BTreeMap<EvmAddress, bool>,
	/// The current voting result of the request.
	pub is_approved: bool,
}

impl From<rollback_requestReturn> for RollbackRequest {
	fn from(value: rollback_requestReturn) -> Self {
		let mut votes = BTreeMap::default();
		for idx in 0..value._6.len() {
			votes.insert(value._6[idx], value._7[idx]);
		}
		let mut txid = value._2;
		txid.reverse();

		Self {
			unsigned_psbt: value._0,
			who: value._1,
			txid: Txid::from_raw_hash(*Hash::from_bytes_ref(&txid)),
			vout: usize::from_str(&value._3.to_string()).unwrap(),
			to: Address::from_str(&value._4).unwrap().assume_checked(),
			amount: Amount::from_sat(u64::from_str(&value._5.to_string()).unwrap()),
			votes,
			is_approved: value._8,
		}
	}
}

pub struct BitcoinRollbackVerifier<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The Bitcoin client.
	btc_client: BtcClient,
	/// The Bifrost client.
	pub bfc_client: Arc<EthClient<F, P, T>>,
	/// The unsigned transaction message sender.
	xt_request_sender: Arc<XtRequestSender>,
	/// The periodic schedule.
	schedule: Schedule,
}

#[async_trait::async_trait]
impl<F, P, TR> RpcApi for BitcoinRollbackVerifier<F, P, TR>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<TR, AnyNetwork>,
	TR: Transport + Clone,
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
impl<F, P, T> XtRequester<F, P, T> for BitcoinRollbackVerifier<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	fn xt_request_sender(&self) -> Arc<XtRequestSender> {
		self.xt_request_sender.clone()
	}

	fn bfc_client(&self) -> Arc<EthClient<F, P, T>> {
		self.bfc_client.clone()
	}
}

#[async_trait::async_trait]
impl<F, P, T> PeriodicWorker for BitcoinRollbackVerifier<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	fn schedule(&self) -> Schedule {
		self.schedule.clone()
	}

	async fn run(&mut self) -> Result<()> {
		loop {
			self.wait_until_next_time().await;

			if self.bfc_client.is_selected_relayer().await? {
				let pending_rollback_psbts = self.get_rollback_psbts().await?;

				log::info!(
					target: &self.bfc_client.get_chain_name(),
					"-[{}] 🔃 Pending rollback psbts: {:?}",
					sub_display_format(SUB_LOG_TARGET),
					pending_rollback_psbts.len()
				);

				let mut stream = tokio_stream::iter(pending_rollback_psbts);
				while let Some(raw_psbt) = stream.next().await {
					let psbt = Psbt::deserialize(&raw_psbt).unwrap();
					let txid = B256::from_str(&psbt.unsigned_tx.txid().to_string()).unwrap();
					let request = self.get_rollback_request(txid).await?;

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
					let (call, metadata) = self.build_unsigned_tx(txid, is_approved).await?;
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

impl<F, P, T> BitcoinRollbackVerifier<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// Instantiates a new `BitcoinRollbackVerifier` instance.
	pub fn new(
		btc_client: BtcClient,
		bfc_client: Arc<EthClient<F, P, T>>,
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
		// confirmations must be satisfied (we consider at least 6 blocks of confirmation)
		if let Some(confirmations) = tx.confirmations {
			if confirmations < 6 {
				return false;
			}
		}
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

		false
	}

	/// Build the payload for the unsigned transaction. (`submit_rollback_poll()`)
	async fn build_payload(
		&self,
		txid: B256,
		is_approved: bool,
	) -> Result<(RollbackPollMessage<AccountId20>, EthereumSignature)> {
		let msg = RollbackPollMessage {
			authority_id: AccountId20(self.bfc_client.address().0 .0),
			txid: H256::from(txid.0),
			is_approved,
		};
		let signature = self
			.bfc_client
			.sign_message(
				&[keccak256("RollbackPoll").as_slice(), txid.as_ref(), &[is_approved as u8]]
					.concat(),
			)
			.await?
			.into();

		Ok((msg, signature))
	}

	/// Build the calldata for the unsigned transaction. (`submit_rollback_poll()`)
	async fn build_unsigned_tx(
		&self,
		txid: B256,
		is_approved: bool,
	) -> Result<(XtRequest, SubmitRollbackPollMetadata)> {
		let (msg, signature) = self.build_payload(txid, is_approved).await?;
		let metadata = SubmitRollbackPollMetadata::new(txid, is_approved);

		Ok((
			XtRequest::from(
				bifrost_runtime::tx().btc_socket_queue().submit_rollback_poll(msg, signature),
			),
			metadata,
		))
	}

	/// Get the pending rollback PSBT's.
	async fn get_rollback_psbts(&self) -> Result<Vec<Bytes>> {
		Ok(self.socket_queue().rollback_psbts().call().await?._0)
	}

	/// Get the rollback information.
	async fn get_rollback_request(&self, txid: B256) -> Result<RollbackRequest> {
		Ok(self.socket_queue().rollback_request(txid).call().await?.into())
	}

	/// Get the `BtcSocketQueue` precompile contract instance.
	fn socket_queue(
		&self,
	) -> &SocketQueueContractInstance<T, Arc<FillProvider<F, P, T, AnyNetwork>>, AnyNetwork> {
		self.bfc_client.protocol_contracts.socket_queue.as_ref().unwrap()
	}
}
