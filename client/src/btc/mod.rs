use bitcoincore_rpc::{
	bitcoin::BlockHash,
	bitcoincore_rpc_json::{BlockRef, GetBlockWithTxsResult},
	Client, RpcApi,
};
use br_primitives::constants::{
	errors::{INVALID_RPC_CALL_ARGS, PROVIDER_INTERNAL_ERROR},
	tx::{DEFAULT_CALL_RETRIES, DEFAULT_CALL_RETRY_INTERVAL_MS},
};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::{sleep, Duration};

pub mod block;
pub mod handlers;
pub mod storage;

pub const LOG_TARGET: &str = "bitcoin";
const SUB_LOG_TARGET: &str = "bitcoin-client";

fn into_json<T>(val: T) -> bitcoincore_rpc::Result<Value>
where
	T: serde::ser::Serialize,
{
	Ok(serde_json::to_value(val)?)
}

/// The core client for Bitcoin chain interactions.
pub struct BtcClient {
	/// The inner client.
	inner: Client,
}

impl BtcClient {
	/// Instantiates a new `BtcClient` instance.
	pub fn new(inner: Client) -> Self {
		Self { inner }
	}

	/// Make a JSON RPC request to the chain provider via the internal connection, and returns the
	/// result. This method wraps the original JSON RPC call and retries whenever the request fails
	/// until it exceeds the maximum retries.
	pub async fn rpc_call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> T {
		let mut retries_remaining: u8 = DEFAULT_CALL_RETRIES;
		let mut error_msg = String::default();

		while retries_remaining > 0 {
			match self.inner.call(cmd, args).await {
				Ok(result) => return result,
				Err(error) => {
					// retry on error
					retries_remaining = retries_remaining.saturating_sub(1);
					error_msg = error.to_string();
				},
			}
			sleep(Duration::from_millis(DEFAULT_CALL_RETRY_INTERVAL_MS)).await;
		}
		panic!(
			"[{}]-[{}] {} [cmd: {}]: {}",
			LOG_TARGET, SUB_LOG_TARGET, PROVIDER_INTERNAL_ERROR, cmd, error_msg
		);
	}

	/// Returns the numbers of block in the longest chain.
	pub async fn get_block_count(&self) -> u64 {
		self.rpc_call("getblockcount", &[]).await
	}

	/// Get block hash at a given height.
	pub async fn get_block_hash(&self, height: u64) -> BlockHash {
		self.rpc_call("getblockhash", &[height.into()]).await
	}

	/// Get block full information by the given block hash.
	pub async fn get_block_info_with_txs(&self, hash: &BlockHash) -> GetBlockWithTxsResult {
		let hash = into_json(hash).expect(INVALID_RPC_CALL_ARGS);
		self.rpc_call("getblock", &[hash, 2.into()]).await
	}

	/// Waits for a specific new block and returns useful info about it.
	/// Returns the current block on timeout or exit.
	///
	/// # Arguments
	///
	/// 1. `timeout`: Time in milliseconds to wait for a response. 0
	/// indicates no timeout.
	pub async fn wait_for_new_block(&self, timeout: u64) -> BlockRef {
		let timeout = into_json(timeout).expect(INVALID_RPC_CALL_ARGS);
		self.rpc_call("waitfornewblock", &[timeout]).await
	}
}
