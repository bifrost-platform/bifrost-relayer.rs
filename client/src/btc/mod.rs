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

pub struct BtcClient {
	client: Client,
}

impl BtcClient {
	pub fn new(client: Client) -> Self {
		Self { client }
	}

	pub async fn rpc_call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> T {
		let mut retries_remaining: u8 = DEFAULT_CALL_RETRIES;
		let mut error_msg = String::default();

		while retries_remaining > 0 {
			match self.client.call(cmd, args).await {
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

	pub async fn get_block_count(&self) -> u64 {
		self.rpc_call("getblockcount", &[]).await
	}

	pub async fn get_block_hash(&self, height: u64) -> BlockHash {
		self.rpc_call("getblockhash", &[height.into()]).await
	}

	pub async fn get_block_info_with_txs(&self, hash: &BlockHash) -> GetBlockWithTxsResult {
		let hash = into_json(hash).expect(INVALID_RPC_CALL_ARGS);
		self.rpc_call("getblock", &[hash, 2.into()]).await
	}

	pub async fn wait_for_new_block(&self, timeout: u64) -> BlockRef {
		let timeout = into_json(timeout).expect(INVALID_RPC_CALL_ARGS);
		self.rpc_call("waitfornewblock", &[timeout]).await
	}
}
