//! The EVM transport of the provider failover (see [`crate::failover`]).
//!
//! It sits under the provider stack, so every call (contract bindings, fillers, the tx
//! heartbeat) goes through it unchanged. A request is resent as is: `eth_sendRawTransaction`
//! carries the signed bytes, so another endpoint gets the very same transaction, and an "already
//! known" answer resolves to its hash, as does a "nonce too low" once the hash is found on chain
//! after an earlier attempt went unanswered. Endpoints are [`HttpEndpoint`]s, which keep a
//! provider failure or a rejected key visible as one.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll},
	time::Duration,
};

use alloy::{
	primitives::{B256, Bytes, U64, keccak256},
	rpc::json_rpc::{
		ErrorPayload, Id, Request, RequestPacket, Response, ResponsePacket, ResponsePayload,
	},
	transports::{RpcError, TransportError, TransportErrorKind, TransportFut},
};
use reqwest::{StatusCode, Url};
use tokio::time::timeout;
use tower::{Service, ServiceExt};

use br_primitives::utils::sub_display_format;

use crate::failover::{Attempt, FailoverHandle, FailoverState, Verdict};

const SUB_LOG_TARGET: &str = "rpc-failover";

/// The outcome of a request on one endpoint.
type Outcome = Result<ResponsePacket, TransportError>;

fn classify_error(err: &TransportError) -> Verdict {
	match err {
		RpcError::ErrorResp(payload) => classify_response(payload),
		RpcError::Transport(kind) => match kind {
			TransportErrorKind::HttpError(e) => Verdict::from_http_status(e.status),
			TransportErrorKind::NonRetryable(_) => Verdict::Rotate,
			// connection failures, timeouts, missing batch responses
			_ => Verdict::Retry,
		},
		// a malformed body (e.g. an html error page from a proxy)
		RpcError::DeserError { .. } => Verdict::Retry,
		RpcError::NullResp
		| RpcError::UnsupportedFeature(_)
		| RpcError::LocalUsageError(_)
		| RpcError::SerError(_) => Verdict::Fatal,
	}
}

fn classify_response(payload: &ErrorPayload) -> Verdict {
	let message = payload.message.to_lowercase();
	if super::is_nonce_too_low(&message) {
		return Verdict::Fatal;
	}
	// checked first: revert reasons are free text ("daily limit exceeded", ...)
	if message.contains("revert") {
		return Verdict::Reject;
	}
	if payload.code == -32601
		|| message.contains("method not found")
		|| message.contains("method not supported")
		|| message.contains("unsupported method")
		|| message.contains("unable to complete request")
		|| message.contains("is not available")
		|| message.contains("missing trie node")
	{
		return Verdict::Unsupported;
	}
	// a node behind the head the caller saw
	if ["header not found", "unknown block", "beyond current head"]
		.iter()
		.any(|pattern| message.contains(pattern))
	{
		return Verdict::Retry;
	}
	// a provider's cap on `eth_getLogs` (Alchemy free tier: a 10-block range; Infura: -32005
	// "query returned more than 10000 results"): this request, not the endpoint, is too large
	// for it, so ask the others without leaving it
	if [
		"block range",
		"range too large",
		"too many blocks",
		"returned more than",
		"too many results",
		"response size",
	]
	.iter()
	.any(|pattern| message.contains(pattern))
	{
		return Verdict::Unsupported;
	}
	if payload.code == 429
		|| payload.code == -32005
		|| [
			"rate limit",
			"too many requests",
			"limit exceeded",
			"timeout",
			"timed out",
			"busy",
			"try again",
		]
		.iter()
		.any(|pattern| message.contains(pattern))
	{
		return Verdict::Retry;
	}
	Verdict::Reject
}

/// The backoff an endpoint asked for in its error response (also one sent with an HTTP error
/// status, like Infura's 429), if any.
fn backoff_hint(outcome: &Outcome) -> Option<Duration> {
	let packet = match outcome {
		Ok(packet) => packet.clone(),
		Err(RpcError::Transport(TransportErrorKind::HttpError(e))) => {
			serde_json::from_str::<ResponsePacket>(&e.body).ok()?
		},
		Err(_) => return None,
	};
	let data = packet.as_error()?.try_data_as::<serde_json::Value>()?.ok()?;
	// if daily rate limit exceeded, infura returns the requested backoff in the error response
	let backoff_seconds = &data["rate"]["backoff_seconds"];
	if let Some(seconds) = backoff_seconds.as_u64() {
		return Some(Duration::from_secs(seconds));
	}
	backoff_seconds.as_f64().map(|seconds| Duration::from_secs(seconds as u64 + 1))
}

/// The id and transaction hash of an `eth_sendRawTransaction` request.
fn raw_transaction(request: &RequestPacket) -> Option<(&Id, B256)> {
	let RequestPacket::Single(request) = request else { return None };
	if request.method() != "eth_sendRawTransaction" {
		return None;
	}
	let (raw,): (Bytes,) = serde_json::from_str(request.params()?.get()).ok()?;
	Some((request.id(), keccak256(&raw)))
}

/// The successful response of an `eth_sendRawTransaction`: the transaction hash.
fn hash_response(id: &Id, hash: B256) -> Option<ResponsePacket> {
	let hash = serde_json::value::to_raw_value(&hash).ok()?;
	Some(ResponsePacket::Single(Response {
		id: id.clone(),
		payload: ResponsePayload::Success(hash),
	}))
}

/// Whether an endpoint answered "already known": the same signed bytes are in its mempool.
fn is_already_known(outcome: &Outcome) -> bool {
	let Some(error) = outcome.as_ref().ok().and_then(|packet| packet.as_error()) else {
		return false;
	};
	let message = error.message.to_lowercase();
	// not "unknown transaction type", a rejection
	["already known", "alreadyknown", "known transaction"]
		.iter()
		.any(|pattern| message.contains(pattern))
		&& !message.contains("unknown")
}

fn describe(outcome: &Outcome) -> String {
	match outcome {
		Ok(packet) => packet.as_error().map(|e| e.to_string()).unwrap_or_default(),
		Err(e) => e.to_string(),
	}
}

/// An HTTP transport like alloy's `Http<reqwest::Client>`, except that a rejected key (401 / 403)
/// or a failing provider (408, 429, 5xx) stays an HTTP error even when its body is a JSON-RPC
/// error (Alchemy: -32600 "Must be authenticated!"), so the failover tells it apart from a
/// rejected request. Its errors leave out the URL, which may hold an API key.
#[derive(Clone, Debug)]
pub struct HttpEndpoint {
	client: reqwest::Client,
	url: Url,
}

impl HttpEndpoint {
	pub fn new(url: Url) -> Self {
		Self { client: reqwest::Client::new(), url }
	}

	async fn send(self, request: RequestPacket) -> Outcome {
		let response = self
			.client
			.post(self.url)
			.json(&request)
			.headers(request.headers())
			.send()
			.await
			// reqwest's message carries the URL, and with it the API key
			.map_err(|e| TransportErrorKind::custom(e.without_url()))?;
		let status = response.status();
		let body = response
			.bytes()
			.await
			.map_err(|e| TransportErrorKind::custom(e.without_url()))?;
		let http_error = || {
			Err(TransportErrorKind::http_error(
				status.as_u16(),
				String::from_utf8_lossy(&body).into_owned(),
			))
		};
		// a rejected key or a failing provider, whatever the body says (a proxy's 503 may carry a
		// JSON-RPC "Internal error")
		if status == StatusCode::UNAUTHORIZED
			|| status == StatusCode::FORBIDDEN
			|| status == StatusCode::REQUEST_TIMEOUT
			|| status == StatusCode::TOO_MANY_REQUESTS
			|| status.is_server_error()
		{
			return http_error();
		}
		if !status.is_success() {
			// e.g. Alchemy's 400 "Unsupported method", classified by the error itself
			return match serde_json::from_slice::<ResponsePacket>(&body) {
				Ok(response) if response.is_error() => Ok(response),
				_ => http_error(),
			};
		}
		serde_json::from_slice(&body)
			.map_err(|err| TransportError::deser_err(err, String::from_utf8_lossy(&body)))
	}
}

impl Service<RequestPacket> for HttpEndpoint {
	type Response = ResponsePacket;
	type Error = TransportError;
	type Future = TransportFut<'static>;

	fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, request: RequestPacket) -> Self::Future {
		Box::pin(self.clone().send(request))
	}
}

/// A transport that spreads one chain's requests over its endpoints. See the module docs.
#[derive(Clone, Debug)]
pub struct FailoverService<S> {
	/// The endpoint transports, primary first.
	endpoints: Arc<[S]>,
	/// The shared failover state.
	state: FailoverHandle,
	/// The retries of a request on one endpoint, after the first attempt.
	max_retries: u32,
	/// The interval between retries, unless the endpoint asks for another.
	retry_interval: Duration,
	/// The timeout of a single attempt.
	call_timeout: Duration,
}

impl<S> FailoverService<S> {
	/// Creates a failover over `endpoints` (URL and transport each), the primary first.
	pub fn new(
		chain_name: String,
		endpoints: Vec<(String, S)>,
		max_retries: u32,
		retry_interval: Duration,
		call_timeout: Duration,
		probe_primary: Duration,
	) -> Self {
		let (urls, endpoints): (Vec<_>, Vec<_>) = endpoints.into_iter().unzip();
		Self {
			endpoints: endpoints.into(),
			state: FailoverState::new(chain_name, &urls, probe_primary),
			max_retries,
			retry_interval,
			call_timeout,
		}
	}

	/// The shared failover state.
	pub fn handle(&self) -> FailoverHandle {
		self.state.clone()
	}
}

impl<S> FailoverService<S>
where
	S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
		+ Clone
		+ Send
		+ Sync
		+ 'static,
	S::Future: Send + 'static,
{
	/// Checks that every fallback serves `chain_id`. Panics on one that serves another chain,
	/// a misconfiguration that would go unnoticed until a failover; one that cannot be reached
	/// now is only warned about, since it may just be down.
	pub async fn verify_fallbacks(&self, chain_id: u64) {
		for index in 1..self.endpoints.len() {
			let url = self.state.url(index);
			let Ok(request) = Request::new("eth_chainId", Id::Number(0), ()).serialize() else {
				continue;
			};
			let served = match self.call_endpoint(index, RequestPacket::Single(request)).await {
				Ok(ResponsePacket::Single(Response {
					payload: ResponsePayload::Success(id),
					..
				})) => serde_json::from_str::<U64>(id.get()).ok(),
				_ => None,
			};
			match served {
				Some(id) if id.to::<u64>() != chain_id => panic!(
					"fallback provider {url} of {} serves chain {id}, not {chain_id}",
					self.state.chain_name()
				),
				Some(_) => {},
				None => log::warn!(
					target: self.state.chain_name(),
					"-[{}] ⚠️  Fallback provider {url} is unreachable: its chain id is unchecked",
					sub_display_format(SUB_LOG_TARGET),
				),
			}
		}
	}

	async fn send(self, request: RequestPacket) -> Outcome {
		// set once an attempt went unanswered: it may have reached a node all the same
		let unanswered = AtomicBool::new(false);
		let what = || request.method_names().collect::<Vec<_>>().join(",");
		self.state
			.execute(what, self.max_retries, self.retry_interval, |index| {
				self.attempt(index, request.clone(), &unanswered)
			})
			.await
	}

	/// Sends `request` to one endpoint, with the timeout.
	async fn call_endpoint(&self, index: usize, request: RequestPacket) -> Outcome {
		let mut endpoint = self.endpoints[index].clone();
		let call = async move { endpoint.ready().await?.call(request).await };
		br_metrics::increase_rpc_calls(self.state.chain_name());
		match timeout(self.call_timeout, call).await {
			Ok(outcome) => outcome,
			Err(_) => Err(TransportErrorKind::custom_str(&format!(
				"request timed out after {:?}",
				self.call_timeout
			))),
		}
	}

	/// Whether the endpoint knows the transaction `hash`.
	async fn is_known(&self, index: usize, hash: B256) -> bool {
		let Ok(lookup) =
			Request::new("eth_getTransactionByHash", Id::Number(0), (hash,)).serialize()
		else {
			return false;
		};
		match self.call_endpoint(index, RequestPacket::Single(lookup)).await {
			Ok(ResponsePacket::Single(Response {
				payload: ResponsePayload::Success(tx), ..
			})) => tx.get() != "null",
			_ => false,
		}
	}

	async fn attempt(
		&self,
		index: usize,
		request: RequestPacket,
		unanswered: &AtomicBool,
	) -> Attempt<Outcome> {
		let outcome = self.call_endpoint(index, request.clone()).await;
		let verdict = match &outcome {
			Ok(packet) => packet.as_error().map(classify_response),
			Err(err) => Some(classify_error(err)),
		};
		let Some(verdict) = verdict else { return Attempt::served(outcome) };

		if let Some((id, hash)) = raw_transaction(&request) {
			// the same bytes are in the mempool already: sent by an attempt that went
			// unanswered, or through another endpoint
			if is_already_known(&outcome)
				&& let Some(response) = hash_response(id, hash)
			{
				return Attempt::served(Ok(response));
			}
			// an unanswered attempt may have landed, using the nonce of this very transaction
			if verdict == Verdict::Fatal
				&& unanswered.load(Ordering::SeqCst)
				&& self.is_known(index, hash).await
				&& let Some(response) = hash_response(id, hash)
			{
				return Attempt::served(Ok(response));
			}
			if outcome.is_err() {
				unanswered.store(true, Ordering::SeqCst);
			}
		}
		Attempt {
			backoff: backoff_hint(&outcome),
			error: describe(&outcome),
			verdict: Some(verdict),
			outcome,
		}
	}
}

impl<S> Service<RequestPacket> for FailoverService<S>
where
	S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
		+ Clone
		+ Send
		+ Sync
		+ 'static,
	S::Future: Send + 'static,
{
	type Response = ResponsePacket;
	type Error = TransportError;
	type Future = TransportFut<'static>;

	fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		// each endpoint is readied when a request reaches it
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, request: RequestPacket) -> Self::Future {
		Box::pin(self.clone().send(request))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::failover::test_node::{closed_port, node};
	use alloy::{
		primitives::{B256, Bytes},
		rpc::json_rpc::{Id, Request},
		transports::HttpError,
	};
	use serde_json::value::RawValue;
	use std::{
		collections::VecDeque,
		sync::{
			Arc, Mutex,
			atomic::{AtomicUsize, Ordering},
		},
	};
	use tokio::time::sleep;

	#[derive(Clone)]
	enum Step {
		Result(&'static str),
		Error(i64, &'static str),
		Http(u16),
		HttpBody(u16, &'static str),
		Hang,
	}

	#[derive(Clone)]
	struct Mock {
		script: Arc<Mutex<VecDeque<Step>>>,
		calls: Arc<AtomicUsize>,
	}

	impl Mock {
		fn new(script: Vec<Step>) -> Self {
			Self {
				script: Arc::new(Mutex::new(script.into())),
				calls: Arc::new(AtomicUsize::new(0)),
			}
		}

		fn calls(&self) -> usize {
			self.calls.load(Ordering::SeqCst)
		}
	}

	impl Service<RequestPacket> for Mock {
		type Response = ResponsePacket;
		type Error = TransportError;
		type Future = TransportFut<'static>;

		fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Ready(Ok(()))
		}

		fn call(&mut self, request: RequestPacket) -> Self::Future {
			self.calls.fetch_add(1, Ordering::SeqCst);
			let step = self.script.lock().unwrap().pop_front().unwrap_or(Step::Http(503));
			let RequestPacket::Single(request) = request else { unreachable!() };
			let id = request.id().clone();
			Box::pin(async move {
				let payload = match step {
					Step::Result(json) => {
						ResponsePayload::Success(RawValue::from_string(json.to_string()).unwrap())
					},
					Step::Error(code, message) => ResponsePayload::Failure(ErrorPayload {
						code,
						message: message.into(),
						data: None,
					}),
					Step::Http(status) => {
						return Err(TransportErrorKind::HttpError(HttpError {
							status,
							body: String::new(),
						})
						.into());
					},
					Step::HttpBody(status, body) => {
						return Err(TransportErrorKind::HttpError(HttpError {
							status,
							body: body.to_string(),
						})
						.into());
					},
					Step::Hang => {
						sleep(Duration::from_secs(3600)).await;
						unreachable!()
					},
				};
				Ok(ResponsePacket::Single(Response { id, payload }))
			})
		}
	}

	fn failover(mocks: &[&Mock], probe_primary: Duration) -> FailoverService<Mock> {
		let endpoints = mocks
			.iter()
			.enumerate()
			.map(|(i, mock)| (format!("https://ep{i}.io"), (*mock).clone()))
			.collect();
		FailoverService::new(
			"test".into(),
			endpoints,
			2,
			Duration::from_millis(1),
			Duration::from_millis(50),
			probe_primary,
		)
	}

	fn request(method: &'static str) -> RequestPacket {
		RequestPacket::Single(Request::new(method, Id::Number(1), ()).serialize().unwrap())
	}

	async fn call(service: &FailoverService<Mock>, method: &'static str) -> Outcome {
		service.clone().call(request(method)).await
	}

	fn result(outcome: &Outcome) -> String {
		match outcome.as_ref().unwrap().single_payload().unwrap() {
			ResponsePayload::Success(raw) => raw.get().to_string(),
			ResponsePayload::Failure(e) => panic!("unexpected error response: {e}"),
		}
	}

	const HOUR: Duration = Duration::from_secs(3600);

	#[tokio::test]
	async fn retries_then_fails_over_and_sticks() {
		let a = Mock::new(vec![Step::Http(503), Step::Http(502), Step::Http(429)]);
		let b = Mock::new(vec![Step::Result("\"0x1\""), Step::Result("\"0x2\"")]);
		let service = failover(&[&a, &b], HOUR);

		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0x1\"");
		assert_eq!(a.calls(), 3, "the primary gets every retry first");
		assert_eq!(service.handle().current_url(), "https://ep1.io");

		// stays on the fallback while the probe window is open
		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0x2\"");
		assert_eq!(a.calls(), 3);
	}

	#[tokio::test]
	async fn probes_the_primary_once_per_window() {
		let a = Mock::new(vec![
			Step::Http(503),
			Step::Http(503),
			Step::Http(503),
			// the probe, still down
			Step::Http(503),
			// the next probe, recovered
			Step::Result("\"0xa\""),
		]);
		let b = Mock::new(vec![Step::Result("\"0x1\""); 3]);
		let service = failover(&[&a, &b], Duration::ZERO);

		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0x1\"");
		// a failed probe is a single attempt
		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0x1\"");
		assert_eq!(a.calls(), 4);
		assert_eq!(service.handle().current_url(), "https://ep1.io");

		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0xa\"");
		assert_eq!(service.handle().current_url(), "https://ep0.io");
	}

	#[tokio::test]
	async fn rejections_are_retried_but_never_fail_over() {
		let a = Mock::new(vec![Step::Error(3, "execution reverted: daily limit exceeded"); 3]);
		let b = Mock::new(vec![]);
		let service = failover(&[&a, &b], HOUR);

		let outcome = call(&service, "eth_call").await;
		assert!(outcome.unwrap().as_error().unwrap().message.contains("reverted"));
		assert_eq!(a.calls(), 3);
		assert_eq!(b.calls(), 0);
		assert_eq!(service.handle().current_url(), "https://ep0.io");
	}

	#[tokio::test]
	async fn nonce_too_low_returns_at_once() {
		let a = Mock::new(vec![Step::Error(-32000, "nonce too low")]);
		let b = Mock::new(vec![]);
		let service = failover(&[&a, &b], HOUR);

		let outcome = call(&service, "eth_sendRawTransaction").await;
		assert!(super::super::is_nonce_too_low(&outcome.unwrap().as_error().unwrap().message));
		assert_eq!(a.calls(), 1);
		assert_eq!(b.calls(), 0);
	}

	#[tokio::test]
	async fn unsupported_methods_try_the_others_without_switching() {
		let a = Mock::new(vec![Step::Error(
			-32601,
			"the method txpool_status does not exist/is not available",
		)]);
		let b = Mock::new(vec![Step::Result("{}")]);
		let service = failover(&[&a, &b], HOUR);

		assert_eq!(result(&call(&service, "txpool_status").await), "{}");
		assert_eq!(a.calls(), 1, "an unsupported method is not retried");
		assert_eq!(service.handle().current_url(), "https://ep0.io");
	}

	#[tokio::test]
	async fn unsupported_everywhere_returns_the_error_response() {
		// the only endpoint is retried, in case the answer was transient
		let a = Mock::new(vec![Step::Error(-32601, "method not found"); 3]);
		let service = failover(&[&a], HOUR);

		let outcome = call(&service, "txpool_status").await;
		assert_eq!(outcome.unwrap().as_error().unwrap().code, -32601);
		assert_eq!(a.calls(), 3);
	}

	#[tokio::test]
	async fn rejected_keys_fail_over_whatever_the_body() {
		let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Must be authenticated!"}}"#;
		let a = Mock::new(vec![Step::HttpBody(401, body)]);
		let b = Mock::new(vec![Step::Result("\"0x1\"")]);
		let service = failover(&[&a, &b], HOUR);

		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0x1\"");
		assert_eq!(a.calls(), 1);
		assert_eq!(service.handle().current_url(), "https://ep1.io");
	}

	#[tokio::test]
	async fn misconfigured_endpoints_fail_over_without_retries() {
		let a = Mock::new(vec![Step::Http(401)]);
		let b = Mock::new(vec![Step::Result("\"0x1\"")]);
		let service = failover(&[&a, &b], HOUR);

		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0x1\"");
		assert_eq!(a.calls(), 1);
		assert_eq!(service.handle().current_url(), "https://ep1.io");
	}

	#[tokio::test]
	async fn timeouts_fail_over() {
		let a = Mock::new(vec![Step::Hang; 3]);
		let b = Mock::new(vec![Step::Result("\"0x1\"")]);
		let service = failover(&[&a, &b], HOUR);

		assert_eq!(result(&call(&service, "eth_blockNumber").await), "\"0x1\"");
		assert_eq!(a.calls(), 3);
	}

	#[tokio::test]
	async fn rate_limit_responses_fail_over() {
		let a = Mock::new(vec![Step::Error(-32005, "limit exceeded"); 3]);
		let b = Mock::new(vec![Step::Result("\"0x1\"")]);
		let service = failover(&[&a, &b], HOUR);

		assert_eq!(result(&call(&service, "eth_getLogs").await), "\"0x1\"");
		assert_eq!(service.handle().current_url(), "https://ep1.io");
	}

	#[tokio::test]
	async fn every_endpoint_failing_returns_the_last_error() {
		let a = Mock::new(vec![Step::Http(503); 3]);
		let b = Mock::new(vec![Step::Http(502); 3]);
		let service = failover(&[&a, &b], HOUR);

		let err = call(&service, "eth_blockNumber").await.unwrap_err();
		assert!(err.to_string().contains("502"), "{err}");
		assert_eq!(service.handle().current_url(), "https://ep0.io");
	}

	#[test]
	fn unknown_transaction_type_is_not_already_known() {
		let outcome = |message: &'static str| -> Outcome {
			Ok(ResponsePacket::Single(Response {
				id: Id::Number(1),
				payload: ResponsePayload::Failure(ErrorPayload {
					code: -32000,
					message: message.into(),
					data: None,
				}),
			}))
		};
		assert!(is_already_known(&outcome("already known")));
		assert!(is_already_known(&outcome("known transaction: 0xabc")));
		assert!(!is_already_known(&outcome("unknown transaction type")));
	}

	#[test]
	fn backoff_hints_are_read_from_http_errors_too() {
		let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32005,"message":"daily request count exceeded","data":{"rate":{"backoff_seconds":86400}}}}"#;
		let outcome: Outcome =
			Err(TransportErrorKind::HttpError(HttpError { status: 429, body: body.to_string() })
				.into());
		assert_eq!(backoff_hint(&outcome), Some(Duration::from_secs(86400)));
	}

	#[tokio::test]
	#[should_panic(expected = "serves chain 56, not 1")]
	async fn fallbacks_of_another_chain_are_refused() {
		let a = Mock::new(vec![]);
		let b = Mock::new(vec![Step::Result("\"0x38\"")]);
		failover(&[&a, &b], HOUR).verify_fallbacks(1).await;
	}

	#[tokio::test]
	async fn fallbacks_of_the_chain_or_unreachable_pass() {
		let a = Mock::new(vec![]);
		let b = Mock::new(vec![Step::Result("\"0x1\"")]);
		let c = Mock::new(vec![Step::Http(503)]);
		failover(&[&a, &b, &c], HOUR).verify_fallbacks(1).await;
		assert_eq!(a.calls(), 0, "the primary is checked elsewhere");
	}

	fn send_raw(raw: &Bytes) -> RequestPacket {
		RequestPacket::Single(
			Request::new("eth_sendRawTransaction", Id::Number(7), (raw.clone(),))
				.serialize()
				.unwrap(),
		)
	}

	#[tokio::test]
	async fn nonce_too_low_after_an_unanswered_send_resolves_to_the_hash_when_known() {
		let raw = Bytes::from_static(&[0x02, 0xf8, 0x01, 0x02, 0x03]);
		// timed out, then the nonce is taken: by this very transaction, the lookup says
		let a = Mock::new(vec![
			Step::Hang,
			Step::Error(-32000, "nonce too low"),
			Step::Result(r#"{"hash":"0x01"}"#),
		]);
		let service = failover(&[&a], HOUR);

		let outcome = service.clone().call(send_raw(&raw)).await;
		let hash: B256 = serde_json::from_str(&result(&outcome)).unwrap();
		assert_eq!(hash, keccak256(&raw));
		assert_eq!(a.calls(), 3);
	}

	#[tokio::test]
	async fn nonce_too_low_after_an_unanswered_send_stays_an_error_when_unknown() {
		let raw = Bytes::from_static(&[0x02, 0xf8, 0x01, 0x02, 0x03]);
		let a =
			Mock::new(vec![Step::Hang, Step::Error(-32000, "nonce too low"), Step::Result("null")]);
		let service = failover(&[&a], HOUR);

		let outcome = service.clone().call(send_raw(&raw)).await;
		assert!(super::super::is_nonce_too_low(&outcome.unwrap().as_error().unwrap().message));
	}

	async fn endpoint_outcome(status: u16, body: &'static str) -> Outcome {
		let (url, _) = node(vec![(status, body)]).await;
		HttpEndpoint::new(url).call(request("eth_blockNumber")).await
	}

	#[tokio::test]
	async fn http_endpoint_keeps_provider_failures_and_rejected_keys() {
		// a failing proxy's JSON-RPC error stays an HTTP error: retried, then failed over
		let internal =
			r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"Internal error"}}"#;
		let err = endpoint_outcome(503, internal).await.unwrap_err();
		assert_eq!(classify_error(&err), Verdict::Retry);

		let unauthenticated = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Must be authenticated!"}}"#;
		let err = endpoint_outcome(401, unauthenticated).await.unwrap_err();
		assert_eq!(classify_error(&err), Verdict::Rotate);

		// a 400 is classified by its JSON-RPC error
		let unsupported = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Unsupported method: txpool_status on ETH_MAINNET"}}"#;
		let packet = endpoint_outcome(400, unsupported).await.unwrap();
		assert_eq!(classify_response(packet.as_error().unwrap()), Verdict::Unsupported);

		let packet = endpoint_outcome(200, r#"{"jsonrpc":"2.0","id":1,"result":"0x1"}"#)
			.await
			.unwrap();
		assert!(packet.is_success());
	}

	#[tokio::test]
	async fn http_endpoint_errors_leave_out_the_url() {
		let mut url = closed_port().await;
		url.set_path("/v2/SECRET");
		let err = HttpEndpoint::new(url).call(request("eth_blockNumber")).await.unwrap_err();
		assert!(!err.to_string().contains("SECRET"), "{err}");
	}

	#[tokio::test]
	async fn already_known_resolves_to_the_transaction_hash() {
		let raw = Bytes::from_static(&[0x02, 0xf8, 0x01, 0x02, 0x03]);
		let a = Mock::new(vec![Step::Http(503), Step::Error(-32000, "already known")]);
		let service = failover(&[&a], HOUR);

		let request = RequestPacket::Single(
			Request::new("eth_sendRawTransaction", Id::Number(7), (raw.clone(),))
				.serialize()
				.unwrap(),
		);
		let outcome = service.clone().call(request).await;
		let hash: B256 = serde_json::from_str(&result(&outcome)).unwrap();
		assert_eq!(hash, keccak256(&raw));
	}

	#[test]
	fn classifies_responses() {
		let response = |code: i64, message: &'static str| ErrorPayload {
			code,
			message: message.into(),
			data: None,
		};
		assert_eq!(classify_response(&response(3, "execution reverted")), Verdict::Reject);
		assert_eq!(
			classify_response(&response(3, "execution reverted: limit exceeded")),
			Verdict::Reject
		);
		assert_eq!(classify_response(&response(-32005, "limit exceeded")), Verdict::Retry);
		assert_eq!(classify_response(&response(429, "Too Many Requests")), Verdict::Retry);
		assert_eq!(classify_response(&response(-32000, "header not found")), Verdict::Retry);
		assert_eq!(classify_response(&response(-32601, "method not found")), Verdict::Unsupported);
		assert_eq!(
			classify_response(&response(
				-32600,
				"Unsupported method: txpool_status on ETH_MAINNET"
			)),
			Verdict::Unsupported
		);
		assert_eq!(classify_response(&response(-32000, "missing trie node")), Verdict::Unsupported);
		assert_eq!(
			classify_response(&response(-32602, "block range extends beyond current head block")),
			Verdict::Retry
		);
		assert_eq!(
			classify_response(&response(
				-32600,
				"Under the Free tier plan, you can make eth_getLogs requests with up to a 10 block range."
			)),
			Verdict::Unsupported
		);
		assert_eq!(classify_response(&response(-32003, "EVM error: OutOfFunds")), Verdict::Reject);
		assert_eq!(
			classify_response(&response(-32005, "query returned more than 10000 results")),
			Verdict::Unsupported
		);
		assert_eq!(classify_response(&response(-32005, "limit exceeded")), Verdict::Retry);
		assert_eq!(classify_response(&response(-32000, "nonce too low")), Verdict::Fatal);
		assert_eq!(
			classify_response(&response(-32000, "insufficient funds for gas")),
			Verdict::Reject
		);
	}

	/// Real nodes from `EVM_FAILOVER_TEST_RPCS`: chains separated by `;`, each with two or more
	/// urls of that chain separated by `,`, primary first:
	/// `EVM_FAILOVER_TEST_RPCS='<a>,<b>;<c>,<d>' cargo test -p br-client evm_live_nodes -- --ignored`
	fn live_chains() -> Vec<Vec<Url>> {
		std::env::var("EVM_FAILOVER_TEST_RPCS")
			.expect("EVM_FAILOVER_TEST_RPCS is not set")
			.split(';')
			.map(|chain| chain.split(',').map(|url| url.trim().parse().unwrap()).collect())
			.collect()
	}

	fn live_provider(
		urls: &[Url],
		probe_primary: Duration,
	) -> (impl alloy::providers::Provider + use<>, FailoverHandle) {
		use alloy::{providers::ProviderBuilder, rpc::client::RpcClient};

		let failover = FailoverService::new(
			"live".into(),
			urls.iter()
				.map(|url| (url.to_string(), HttpEndpoint::new(url.clone())))
				.collect(),
			1,
			Duration::from_millis(300),
			Duration::from_secs(20),
			probe_primary,
		);
		let handle = failover.handle();
		let client = RpcClient::builder().transport(failover, false);
		(ProviderBuilder::new().connect_client(client), handle)
	}

	#[tokio::test]
	#[ignore = "needs real nodes in EVM_FAILOVER_TEST_RPCS"]
	async fn evm_live_nodes() {
		use alloy::{
			network::TransactionBuilder,
			primitives::{Address, bytes},
			providers::Provider,
			rpc::types::{Filter, TransactionRequest},
		};
		use std::time::Instant;

		for urls in live_chains() {
			assert!(urls.len() >= 2, "every chain needs at least two urls");
			for order in [urls.clone(), urls.iter().rev().cloned().collect::<Vec<_>>()] {
				let (provider, failover) = live_provider(&order, HOUR);
				let primary = failover.current_url().to_string();
				println!("== primary {primary}");

				let chain_id = provider.get_chain_id().await.unwrap();
				let head = provider.get_block_number().await.unwrap();
				provider.get_block_by_number((head - 5).into()).await.unwrap().unwrap();
				provider.get_gas_price().await.unwrap();
				println!("reads ok: chain {chain_id} at #{head}");
				FailoverService::new(
					"live".into(),
					order
						.iter()
						.map(|url| (url.to_string(), HttpEndpoint::new(url.clone())))
						.collect(),
					1,
					Duration::from_millis(300),
					Duration::from_secs(20),
					HOUR,
				)
				.verify_fallbacks(chain_id)
				.await;
				println!("fallbacks serve chain {chain_id}");

				// a revert is an answer: the primary stays
				let reverting_deploy =
					TransactionRequest::default().with_input(bytes!("60006000fd"));
				let started = Instant::now();
				let err = provider.call(reverting_deploy).await.unwrap_err();
				println!("revert ({:?}): {err}", started.elapsed());
				assert!(err.to_string().contains("revert"), "{err}");
				assert_eq!(failover.current_url(), primary);

				// logs over 100 blocks: an endpoint with a lower range cap asks another (of one
				// address, to keep the answer small on busy chains)
				let started = Instant::now();
				let logs = provider
					.get_logs(
						&Filter::new()
							.address(Address::repeat_byte(0xbf))
							.from_block(head - 120)
							.to_block(head - 20),
					)
					.await;
				println!(
					"logs over 100 blocks ({:?}): {}",
					started.elapsed(),
					logs.as_ref()
						.map(|l| format!("{} logs", l.len()))
						.unwrap_or_else(|e| e.to_string())
				);
				assert!(logs.is_ok());
				assert_eq!(failover.current_url(), primary);

				// txpool may be missing anywhere: the primary stays either way
				let started = Instant::now();
				let txpool =
					provider.raw_request::<_, serde_json::Value>("txpool_status".into(), ()).await;
				println!(
					"txpool_status ({:?}): {}",
					started.elapsed(),
					txpool.map(|v| v.to_string()).unwrap_or_else(|e| e.to_string())
				);
				assert_eq!(failover.current_url(), primary);

				// an undecodable raw tx is rejected without failing over
				let err = provider
					.raw_request::<_, B256>("eth_sendRawTransaction".into(), ("0x00",))
					.await
					.unwrap_err();
				println!("invalid raw tx: {err}");
				assert_eq!(failover.current_url(), primary);

				// logs past the head: an error, or an empty answer on some nodes (geth)
				let started = Instant::now();
				let logs = provider
					.get_logs(
						&Filter::new().from_block(head + 1_000_000).to_block(head + 1_000_001),
					)
					.await;
				println!(
					"logs past the head ({:?}): {} → now on {}",
					started.elapsed(),
					logs.map(|l| format!("{} logs", l.len())).unwrap_or_else(|e| e.to_string()),
					failover.current_url()
				);

				// a broken path (a rejected key) fails over
				let mut broken = order[0].clone();
				broken.path_segments_mut().unwrap().pop().push("invalidkey");
				let (provider, failover) = live_provider(&[broken, order[1].clone()], HOUR);
				let started = Instant::now();
				assert_eq!(provider.get_chain_id().await.unwrap(), chain_id);
				println!("broken path → {} ({:?})", failover.current_url(), started.elapsed());
				assert_ne!(failover.current_url(), primary);
			}

			// an unreachable primary fails over; a due probe tries it once and stays away
			let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
				.await
				.unwrap()
				.local_addr()
				.unwrap();
			let dead = Url::parse(&format!("http://{closed}/")).unwrap();
			let (provider, failover) = live_provider(&[dead, urls[0].clone()], Duration::ZERO);
			let started = Instant::now();
			provider.get_block_number().await.unwrap();
			println!("unreachable → {} ({:?})", failover.current_url(), started.elapsed());
			assert!(!failover.current_url().starts_with("http://127.0.0.1"));
			let started = Instant::now();
			provider.get_block_number().await.unwrap();
			println!("probe of the dead primary ({:?})", started.elapsed());
			assert!(!failover.current_url().starts_with("http://127.0.0.1"));
		}
	}
}
