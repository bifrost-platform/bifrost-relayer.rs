//! A Bitcoin Core JSON-RPC client over the provider failover (see [`crate::failover`]).
//!
//! It implements [`RpcApi`], so every call (`getblockcount`, `getrawtransaction`,
//! `sendrawtransaction`, ...) goes through the failover. Unlike `bitcoincore_rpc::Client`, it
//! reads the `error` of a response, which tells a failing node apart from a rejected request; an
//! RPC error is returned as `Error::JsonRpc(jsonrpc::Error::Rpc(_))`.

use std::time::Duration;

use bitcoincore_rpc::{
	Error, RpcApi,
	jsonrpc::{self, error::RpcError},
};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json, value::RawValue};

use br_primitives::utils::sub_display_format;

use crate::failover::{Attempt, FailoverHandle, FailoverState, Verdict};

const SUB_LOG_TARGET: &str = "rpc-failover";

/// The body of a JSON-RPC response.
#[derive(Deserialize)]
struct RpcResponse {
	result: Option<Box<RawValue>>,
	error: Option<RpcError>,
}

fn classify_rpc_error(error: &RpcError) -> Verdict {
	let message = error.message.to_lowercase();
	match error.code {
		// RPC_IN_WARMUP: the node is still loading
		-28 => Verdict::Retry,
		// RPC_METHOD_NOT_FOUND
		-32601 => Verdict::Unsupported,
		// a hosted provider's opaque answer (Alchemy: "Unable to complete request at this
		// time."), given for an unknown tx or block as well: ask the others, which may tell
		_ if message.contains("unable to complete request") => Verdict::Unsupported,
		// a hosted provider's method allowlist (Alchemy: "Unsupported method: ...")
		_ if message.contains("unsupported method") => Verdict::Unsupported,
		// RPC_WALLET_NOT_FOUND, RPC_WALLET_NOT_SPECIFIED: the endpoint lacks the configured wallet
		-18 | -19 => Verdict::Rotate,
		// e.g. `getrawtransaction` of a confirmed tx on a node without `-txindex`, or a block
		// a pruned node no longer has
		_ if message.contains("txindex") || message.contains("pruned") => Verdict::Unsupported,
		// a block past this node's tip: it is behind the one that reported the tip
		_ if message.contains("block height out of range")
			|| message.contains("block not found") =>
		{
			Verdict::Retry
		},
		_ if ["rate limit", "too many requests", "limit exceeded", "limit reached", "quota"]
			.iter()
			.any(|pattern| message.contains(pattern)) =>
		{
			Verdict::Retry
		},
		// RPC_CLIENT_NOT_CONNECTED, RPC_CLIENT_IN_INITIAL_DOWNLOAD: the node is not ready
		-9 | -10 => Verdict::Retry,
		// the JSON-RPC internal and server errors: the provider failed, not the request
		-32603 | -32099..=-32000 => Verdict::Retry,
		// Bitcoin Core rejected the request itself (a bad parameter, an unknown or invalid tx,
		// one already in the chain): it gets the same answer again
		-3 | -5 | -8 | -22 | -25 | -26 | -27 => Verdict::Fatal,
		// anything else (e.g. RPC_MISC_ERROR): retried on this endpoint
		_ => Verdict::Reject,
	}
}

fn failed<T>(verdict: Verdict, error: Error) -> Attempt<bitcoincore_rpc::Result<T>> {
	let message = error.to_string();
	Attempt::failed(verdict, Err(error), message)
}

/// A Bitcoin Core JSON-RPC client that fails over between endpoints.
#[derive(Clone, Debug)]
pub struct FailoverClient {
	/// The HTTP client.
	http: reqwest::Client,
	/// The endpoint URLs (with their credentials), primary first.
	endpoints: Vec<Url>,
	/// The wallet requests to the primary are made against, if any.
	wallet: Option<String>,
	/// The retries of a request on one endpoint, after the first attempt.
	max_retries: u32,
	/// The interval between retries.
	retry_interval: Duration,
	/// The shared failover state.
	state: FailoverHandle,
}

impl FailoverClient {
	/// Creates a client over `endpoints` (with their credentials), the primary first.
	pub fn new(
		chain_name: String,
		endpoints: Vec<Url>,
		wallet: Option<String>,
		max_retries: u32,
		retry_interval: Duration,
		call_timeout: Duration,
		probe_primary: Duration,
	) -> Self {
		let urls: Vec<_> = endpoints.iter().map(Url::to_string).collect();
		Self {
			http: reqwest::Client::builder().timeout(call_timeout).build().unwrap(),
			endpoints,
			wallet,
			max_retries,
			retry_interval,
			state: FailoverState::new(chain_name, &urls, probe_primary),
		}
	}

	/// The shared failover state.
	pub fn failover(&self) -> &FailoverHandle {
		&self.state
	}

	/// The network (`main`, `test`, `testnet4`, ...) the endpoint at `index` serves, if it
	/// answers.
	async fn network(&self, index: usize) -> Option<String> {
		let info = self.attempt::<Value>(index, "getblockchaininfo", &[]).await.outcome.ok()?;
		info["chain"].as_str().map(str::to_string)
	}

	/// Checks that every fallback serves the primary's network. Errs on one that serves
	/// another, a misconfiguration that would go unnoticed until a failover; one that cannot be
	/// reached now (or a primary that cannot) is only warned about, since it may just be down.
	pub async fn verify_fallbacks(&self) -> eyre::Result<()> {
		if !self.state.has_fallbacks() {
			return Ok(());
		}
		let warn = |index: usize| {
			log::warn!(
				target: self.state.chain_name(),
				"-[{}] ⚠️  Provider {} is unreachable: the fallbacks' network is unchecked against it",
				sub_display_format(SUB_LOG_TARGET),
				self.state.url(index),
			)
		};
		let Some(primary) = self.network(0).await else {
			warn(0);
			return Ok(());
		};
		for index in 1..self.endpoints.len() {
			match self.network(index).await {
				Some(network) if network != primary => {
					return Err(eyre::eyre!(
						"fallback provider {} serves {network}, not the primary's {primary}",
						self.state.url(index)
					));
				},
				Some(_) => {},
				None => warn(index),
			}
		}
		Ok(())
	}

	async fn attempt<T: DeserializeOwned>(
		&self,
		index: usize,
		cmd: &str,
		args: &[Value],
	) -> Attempt<bitcoincore_rpc::Result<T>> {
		let mut url = self.endpoints[index].clone();
		// the operator's own node: a hosted fallback has no such wallet
		if index == 0
			&& let Some(wallet) = &self.wallet
		{
			let path = format!("{}/wallet/{wallet}", url.path().trim_end_matches('/'));
			url.set_path(&path);
		}
		let body =
			json!({ "jsonrpc": "1.0", "id": "bifrost-relayer", "method": cmd, "params": args });
		br_metrics::increase_rpc_calls(self.state.chain_name());

		// connection failures and timeouts (the crate's `ReqwestError` is of another reqwest).
		// reqwest's message carries the URL, and with it the credentials or API key
		let response = match self.http.post(url).json(&body).send().await {
			Ok(response) => response,
			Err(e) => {
				return failed(Verdict::Retry, Error::Io(std::io::Error::other(e.without_url())));
			},
		};
		let status = response.status();
		let body = match response.bytes().await {
			Ok(body) => body,
			Err(e) => {
				return failed(Verdict::Retry, Error::Io(std::io::Error::other(e.without_url())));
			},
		};

		// a rejected key, whatever the body says
		if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
			return failed(Verdict::Rotate, Error::ReturnedError(format!("HTTP {status}")));
		}
		// Bitcoin Core answers an RPC error with a non-2xx status and a JSON body
		match serde_json::from_slice::<RpcResponse>(&body) {
			Ok(RpcResponse { error: Some(error), .. }) => {
				failed(classify_rpc_error(&error), Error::JsonRpc(jsonrpc::Error::Rpc(error)))
			},
			Ok(RpcResponse { result, error: None }) if status.is_success() => {
				let parsed = match result {
					Some(result) => serde_json::from_str(result.get()),
					None => serde_json::from_value(Value::Null),
				};
				match parsed {
					Ok(result) => Attempt::served(Ok(result)),
					// the node answered, with a result of another shape
					Err(e) => failed(Verdict::Fatal, Error::Json(e)),
				}
			},
			_ => {
				let body = String::from_utf8_lossy(&body);
				let body = body.chars().take(200).collect::<String>();
				failed(
					Verdict::from_http_status(status.as_u16()),
					Error::ReturnedError(format!("HTTP {status}: {body}")),
				)
			},
		}
	}
}

#[async_trait::async_trait]
impl RpcApi for FailoverClient {
	async fn call<T: for<'a> Deserialize<'a> + Send>(
		&self,
		cmd: &str,
		args: &[Value],
	) -> bitcoincore_rpc::Result<T> {
		self.state
			.execute(
				|| cmd.to_string(),
				self.max_retries,
				self.retry_interval,
				|index| self.attempt(index, cmd, args),
			)
			.await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::failover::test_node::{closed_port, node};
	use tokio::net::TcpListener;

	fn client(endpoints: Vec<Url>, wallet: Option<&str>) -> FailoverClient {
		FailoverClient::new(
			"bitcoin".into(),
			endpoints,
			wallet.map(str::to_string),
			2,
			Duration::from_millis(1),
			Duration::from_secs(5),
			Duration::from_secs(3600),
		)
	}

	#[tokio::test]
	async fn reads_results_against_the_wallet() {
		let (url, requests) = node(vec![(200, r#"{"result":840000,"error":null,"id":"x"}"#)]).await;
		let client = client(vec![url], Some("relayer"));

		assert_eq!(client.get_block_count().await.unwrap(), 840000);
		let request = requests.lock().unwrap()[0].clone();
		assert!(request.starts_with("POST /wallet/relayer "), "{request}");
		assert!(request.contains(r#""method":"getblockcount""#), "{request}");
	}

	#[tokio::test]
	async fn rpc_errors_are_returned_without_failing_over() {
		let (primary, _) = node(vec![(
			500,
			r#"{"result":null,"error":{"code":-5,"message":"No such mempool or blockchain transaction"},"id":"x"}"#,
		)])
		.await;
		let (fallback, fallback_requests) = node(vec![]).await;
		let client = client(vec![primary, fallback], None);

		let err = client.call::<Value>("getrawtransaction", &[json!("00")]).await.unwrap_err();
		assert!(matches!(&err, Error::JsonRpc(jsonrpc::Error::Rpc(e)) if e.code == -5), "{err:?}");
		assert!(fallback_requests.lock().unwrap().is_empty());
	}

	#[tokio::test]
	async fn unreachable_primary_fails_over() {
		// a port nothing listens on
		let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
		let primary = Url::parse(&format!("http://{closed}/")).unwrap();
		let (fallback, _) = node(vec![(200, r#"{"result":7,"error":null,"id":"x"}"#)]).await;
		let client = client(vec![primary, fallback.clone()], None);

		assert_eq!(client.get_block_count().await.unwrap(), 7);
		assert_eq!(client.failover().current_url(), fallback.as_str().trim_end_matches('/'));
	}

	#[tokio::test]
	async fn the_wallet_is_only_on_the_primary() {
		let (primary, _) = node(vec![(401, "")]).await;
		let (fallback, requests) = node(vec![(200, r#"{"result":7,"error":null,"id":"x"}"#)]).await;
		let client = client(vec![primary, fallback], Some("relayer"));

		assert_eq!(client.get_block_count().await.unwrap(), 7);
		let request = requests.lock().unwrap()[0].clone();
		assert!(request.starts_with("POST / "), "{request}");
	}

	#[tokio::test]
	async fn fallbacks_of_another_network_are_refused() {
		let info = |chain: &'static str| -> &'static str {
			match chain {
				"main" => r#"{"result":{"chain":"main"},"error":null,"id":"x"}"#,
				_ => r#"{"result":{"chain":"testnet4"},"error":null,"id":"x"}"#,
			}
		};
		let (primary, _) = node(vec![(200, info("main"))]).await;
		let (fallback, _) = node(vec![(200, info("testnet4"))]).await;
		let err = client(vec![primary, fallback], None).verify_fallbacks().await.unwrap_err();
		assert!(err.to_string().contains("serves testnet4"), "{err}");

		let (primary, _) = node(vec![(200, info("main"))]).await;
		let (fallback, _) = node(vec![(200, info("main"))]).await;
		client(vec![primary, fallback], None).verify_fallbacks().await.unwrap();

		// an unreachable fallback is only warned about
		let (primary, _) = node(vec![(200, info("main"))]).await;
		client(vec![primary, closed_port().await], None)
			.verify_fallbacks()
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn errors_leave_out_the_url() {
		let mut url = closed_port().await;
		url.set_path("/v2/SECRET");
		let client = client(vec![url], None);

		let err = client.get_block_count().await.unwrap_err();
		assert!(!err.to_string().contains("SECRET"), "{err}");
	}

	#[tokio::test]
	async fn unauthorized_endpoint_rotates() {
		let (primary, primary_requests) = node(vec![(401, "")]).await;
		let (fallback, _) = node(vec![(200, r#"{"result":7,"error":null,"id":"x"}"#)]).await;
		let client = client(vec![primary, fallback], None);

		assert_eq!(client.get_block_count().await.unwrap(), 7);
		assert_eq!(primary_requests.lock().unwrap().len(), 1, "no retries on a 401");
	}

	fn rpc_error(code: i32, message: &str) -> RpcError {
		RpcError { code, message: message.to_string(), data: None }
	}

	#[test]
	fn classifies_rpc_errors() {
		assert_eq!(classify_rpc_error(&rpc_error(-28, "Loading block index…")), Verdict::Retry);
		assert_eq!(
			classify_rpc_error(&rpc_error(-32601, "Method not found")),
			Verdict::Unsupported
		);
		assert_eq!(
			classify_rpc_error(&rpc_error(
				-5,
				"No such mempool transaction. Use -txindex or provide a block hash to enable blockchain transaction queries."
			)),
			Verdict::Unsupported
		);
		assert_eq!(classify_rpc_error(&rpc_error(-8, "Block height out of range")), Verdict::Retry);
		assert_eq!(
			classify_rpc_error(&rpc_error(-32001, "Unable to complete request at this time.")),
			Verdict::Unsupported
		);
		assert_eq!(
			classify_rpc_error(&rpc_error(
				-32600,
				"Unsupported method: getwalletinfo on BITCOIN_TESTNET4"
			)),
			Verdict::Unsupported
		);
		assert_eq!(classify_rpc_error(&rpc_error(-5, "Block not found")), Verdict::Retry);
		assert_eq!(
			classify_rpc_error(&rpc_error(-1, "Block not available (pruned data)")),
			Verdict::Unsupported
		);
		assert_eq!(
			classify_rpc_error(&rpc_error(-5, "No such mempool or blockchain transaction")),
			Verdict::Fatal
		);
		assert_eq!(
			classify_rpc_error(&rpc_error(-18, "Requested wallet does not exist or is not loaded")),
			Verdict::Rotate
		);
		assert_eq!(
			classify_rpc_error(&rpc_error(-27, "Transaction already in block chain")),
			Verdict::Fatal
		);
		assert_eq!(classify_rpc_error(&rpc_error(-32603, "Internal error")), Verdict::Retry);
		assert_eq!(classify_rpc_error(&rpc_error(-32000, "upstream timeout")), Verdict::Retry);
		assert_eq!(classify_rpc_error(&rpc_error(-32007, "request limit reached")), Verdict::Retry);
		assert_eq!(
			classify_rpc_error(&rpc_error(-10, "Bitcoin is downloading blocks...")),
			Verdict::Retry
		);
		assert_eq!(classify_rpc_error(&rpc_error(-1, "std::exception")), Verdict::Reject);
	}

	#[test]
	fn classifies_statuses() {
		assert_eq!(Verdict::from_http_status(401), Verdict::Rotate);
		assert_eq!(Verdict::from_http_status(404), Verdict::Rotate);
		assert_eq!(Verdict::from_http_status(429), Verdict::Retry);
		assert_eq!(Verdict::from_http_status(502), Verdict::Retry);
	}

	/// Real nodes, primary first, from `BTC_FAILOVER_TEST_RPCS` (comma separated, at least two
	/// of the same network):
	/// `BTC_FAILOVER_TEST_RPCS=<url>,<url> cargo test -p br-client live_nodes -- --ignored`
	fn live_rpcs() -> Vec<Url> {
		std::env::var("BTC_FAILOVER_TEST_RPCS")
			.expect("BTC_FAILOVER_TEST_RPCS is not set")
			.split(',')
			.map(|url| url.trim().parse().unwrap())
			.collect()
	}

	fn live_client(endpoints: Vec<Url>) -> FailoverClient {
		FailoverClient::new(
			"bitcoin".into(),
			endpoints,
			None,
			1,
			Duration::from_millis(500),
			Duration::from_secs(20),
			Duration::from_secs(3600),
		)
	}

	#[tokio::test]
	#[ignore = "needs real nodes in BTC_FAILOVER_TEST_RPCS"]
	async fn live_nodes() {
		use bitcoincore_rpc::bitcoin::{Txid, hashes::Hash};
		use std::time::Instant;

		let rpcs = live_rpcs();
		assert!(rpcs.len() >= 2, "BTC_FAILOVER_TEST_RPCS needs at least two urls");

		for (a, b) in [(rpcs[0].clone(), rpcs[1].clone()), (rpcs[1].clone(), rpcs[0].clone())] {
			let client = live_client(vec![a.clone(), b.clone()]);
			let primary = client.failover().current_url().to_string();
			println!("== primary {primary}");

			let height = client.get_block_count().await.unwrap();
			let hash = client.get_block_hash(height - 1).await.unwrap();
			let block = client.get_block_header_info(&hash).await.unwrap();
			assert_eq!(block.height as u64, height - 1);
			client.estimate_smart_fee(1, None).await.unwrap();
			println!("reads ok at #{height}");
			client.verify_fallbacks().await.unwrap();
			println!("fallbacks serve the primary's network");

			// an unknown tx is an answer, not a failure: the primary stays
			let started = Instant::now();
			let err = client.get_raw_transaction(&Txid::all_zeros(), None).await.unwrap_err();
			println!("unknown tx ({:?}): {err}", started.elapsed());
			assert!(
				matches!(&err, Error::JsonRpc(jsonrpc::Error::Rpc(e)) if e.code == -5),
				"{err:?}"
			);
			assert_eq!(client.failover().current_url(), primary);

			// a block past the tip fails everywhere: the primary stays
			let started = Instant::now();
			let err = client.get_block_hash(height + 1_000_000).await.unwrap_err();
			println!("past the tip ({:?}): {err}", started.elapsed());
			assert!(
				matches!(&err, Error::JsonRpc(jsonrpc::Error::Rpc(e)) if e.code == -8),
				"{err:?}"
			);
			assert_eq!(client.failover().current_url(), primary);

			// an invalid tx is rejected without failing over
			let err =
				client.call::<Value>("testmempoolaccept", &[json!(["00"])]).await.unwrap_err();
			println!("invalid tx: {err}");
			assert_eq!(client.failover().current_url(), primary);

			// a rejected key fails over to the other node
			let mut bad_key = a.clone();
			bad_key.path_segments_mut().unwrap().pop().push("invalidkey");
			let client = live_client(vec![bad_key, b.clone()]);
			let started = Instant::now();
			assert_eq!(client.get_block_count().await.unwrap(), height);
			println!(
				"rejected key → {} ({:?})",
				client.failover().current_url(),
				started.elapsed()
			);
			assert_ne!(client.failover().current_url(), primary);
		}

		// an unreachable primary fails over
		let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.unwrap()
			.local_addr()
			.unwrap();
		let dead = Url::parse(&format!("http://{closed}/")).unwrap();
		let client = live_client(vec![dead, rpcs[0].clone()]);
		let started = Instant::now();
		client.get_block_count().await.unwrap();
		println!("unreachable → {} ({:?})", client.failover().current_url(), started.elapsed());
		assert!(!client.failover().current_url().starts_with("http://127.0.0.1"));
	}
}
