use std::{sync::Arc, time::Duration};

use eyre::Result;
use miniscript::bitcoin::{BlockHash, Transaction, Txid};

pub use corepc_client::client_sync::Auth;
use corepc_client::client_sync::v31::Client as RawClient;
pub use corepc_client::types::v31::{
	EstimateSmartFee, GetBlockVerboseTwo, GetBlockchainInfo, GetRawTransactionVerbose,
	MempoolAcceptance, RawTransactionOutput,
};

/// An async wrapper around the (blocking) `corepc-client` JSON-RPC client.
///
/// `corepc-client` is synchronous, so every request is dispatched on the blocking
/// thread pool via [`tokio::task::spawn_blocking`]. Requests are retried up to
/// `retries` times, sleeping `retry_interval_ms` between attempts.
#[derive(Clone)]
pub struct BtcClient {
	inner: Arc<RawClient>,
	retries: u32,
	retry_interval_ms: u64,
}

impl BtcClient {
	/// Instantiates a new `BtcClient`. When a `wallet` name is given, requests are
	/// routed to the corresponding wallet endpoint.
	pub fn new(
		url: &str,
		auth: Auth,
		wallet: Option<String>,
		retries: u32,
		retry_interval_ms: u64,
	) -> Result<Self> {
		let url = match wallet {
			Some(wallet) if !wallet.is_empty() => {
				format!("{}/wallet/{}", url.trim_end_matches('/'), wallet)
			},
			_ => url.to_string(),
		};

		let inner = if matches!(auth, Auth::None) {
			RawClient::new(&url)
		} else {
			RawClient::new_with_auth(&url, auth).map_err(|error| eyre::eyre!(error.to_string()))?
		};

		Ok(Self { inner: Arc::new(inner), retries: retries.max(1), retry_interval_ms })
	}

	/// Returns a handle to the same node configured with a different retry count.
	pub fn with_retries(&self, retries: u32) -> Self {
		Self {
			inner: self.inner.clone(),
			retries: retries.max(1),
			retry_interval_ms: self.retry_interval_ms,
		}
	}

	/// Runs a blocking RPC closure on the blocking pool, retrying on failure.
	///
	/// The closure receives the underlying synchronous client and may call any of
	/// its typed methods; its result is awaited and returned. This is the single
	/// place that owns the `spawn_blocking` + retry machinery.
	async fn exec<T, F>(&self, f: F) -> Result<T>
	where
		F: Fn(&RawClient) -> Result<T> + Send + Clone + 'static,
		T: Send + 'static,
	{
		let mut last_error = None;
		for attempt in 0..self.retries {
			let inner = self.inner.clone();
			let f = f.clone();
			match tokio::task::spawn_blocking(move || f(&inner)).await {
				Ok(Ok(value)) => return Ok(value),
				Ok(Err(error)) => last_error = Some(error),
				Err(join_error) => last_error = Some(eyre::eyre!(join_error.to_string())),
			}

			if attempt + 1 < self.retries {
				tokio::time::sleep(Duration::from_millis(self.retry_interval_ms)).await;
			}
		}

		Err(last_error.unwrap_or_else(|| eyre::eyre!("Bitcoin RPC call failed")))
	}

	pub async fn get_block_count(&self) -> Result<u64> {
		self.exec(|client| Ok(client.get_block_count()?.0)).await
	}

	pub async fn get_blockchain_info(&self) -> Result<GetBlockchainInfo> {
		self.exec(|client| Ok(client.get_blockchain_info()?)).await
	}

	pub async fn get_block_hash(&self, height: u64) -> Result<BlockHash> {
		self.exec(move |client| Ok(client.get_block_hash(height)?.block_hash()?)).await
	}

	pub async fn get_block_info_with_txs(
		&self,
		block_hash: &BlockHash,
	) -> Result<GetBlockVerboseTwo> {
		let block_hash = *block_hash;
		self.exec(move |client| Ok(client.get_block_verbose_two(block_hash)?)).await
	}

	/// Fetches the raw transaction. Used purely as an existence check; the decoded
	/// payload is discarded.
	pub async fn get_raw_transaction(&self, txid: &Txid) -> Result<()> {
		let txid = *txid;
		self.exec(move |client| {
			client.get_raw_transaction(txid)?;
			Ok(())
		})
		.await
	}

	pub async fn get_raw_transaction_info(&self, txid: &Txid) -> Result<GetRawTransactionVerbose> {
		let txid = *txid;
		self.exec(move |client| Ok(client.get_raw_transaction_verbose(txid)?)).await
	}

	pub async fn test_mempool_accept(&self, txs: &[Transaction]) -> Result<Vec<MempoolAcceptance>> {
		let txs = txs.to_vec();
		self.exec(move |client| Ok(client.test_mempool_accept(&txs)?.0)).await
	}

	pub async fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid> {
		let tx = tx.clone();
		self.exec(move |client| Ok(client.send_raw_transaction(&tx)?.into_model()?.0))
			.await
	}

	pub async fn estimate_smart_fee(&self, conf_target: u16) -> Result<EstimateSmartFee> {
		self.exec(move |client| Ok(client.estimate_smart_fee(conf_target as u32)?))
			.await
	}
}
