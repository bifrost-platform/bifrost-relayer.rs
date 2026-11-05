//! Provider failover shared by the EVM and Bitcoin clients.
//!
//! A chain has an ordered list of endpoints (`provider`, then `fallback_providers`). Requests
//! stick to the endpoint currently in use and retry on it, then move on to the next endpoint once
//! the retries are exhausted. After `probe_primary` on a fallback, one request tries the primary
//! again (a single attempt) and switches back to it if it answers.
//!
//! ```text
//! request:
//!   order = from the current endpoint on, or the primary first when a probe is due
//!   for each endpoint in order:
//!       attempt it, then by verdict of the outcome:
//!         served / Fatal → return it
//!         Reject         → retry on this endpoint, then return it
//!         Retry          → retry on this endpoint, then try the next one
//!         Rotate         → try the next one
//!         Unsupported    → try the next one, without switching to it
//!   every endpoint failed → return the last outcome (of an endpoint that could answer, if any)
//! ```
//!
//! [`HeadMonitor`] covers what errors cannot: an endpoint that answers, but with a head that is
//! behind or stuck.

use std::{
	future::Future,
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
	},
	time::{Duration, Instant},
};

use br_primitives::utils::sub_display_format;
use tokio::time::sleep;

const SUB_LOG_TARGET: &str = "rpc-failover";

/// What to do after a request failed on an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
	/// The endpoint failed (network, timeout, rate limit, 5xx, behind the head): retry on it,
	/// then fail over.
	Retry,
	/// The endpoint cannot serve any request (auth, not found): fail over without retrying.
	Rotate,
	/// The endpoint cannot answer this request (a method or data it lacks, e.g. `txpool_*`,
	/// pruned state, no txindex, or a hosted provider's opaque error): try the others for this
	/// request only, staying on the current endpoint.
	Unsupported,
	/// A healthy endpoint rejected the request (revert, bad params): retry on it, but never fail
	/// over since every endpoint would answer the same.
	Reject,
	/// Retrying cannot succeed (`nonce too low`, an unknown tx, a malformed result): return at
	/// once.
	Fatal,
}

impl Verdict {
	/// The verdict of an HTTP error status: the provider failing (408, 429, 5xx) or the
	/// endpoint misconfigured (401, 403, 404, ...).
	pub fn from_http_status(status: u16) -> Self {
		if status == 408 || status == 429 || status >= 500 { Self::Retry } else { Self::Rotate }
	}
}

/// One attempt of a request on one endpoint.
pub struct Attempt<O> {
	/// The outcome handed back to the caller if this attempt ends the request.
	pub outcome: O,
	/// `None` when the endpoint served the request.
	pub verdict: Option<Verdict>,
	/// The backoff the endpoint asked for, if any.
	pub backoff: Option<Duration>,
	/// The failure, for logs.
	pub error: String,
}

impl<O> Attempt<O> {
	/// The endpoint served the request.
	pub fn served(outcome: O) -> Self {
		Self { outcome, verdict: None, backoff: None, error: String::new() }
	}

	/// The request failed on the endpoint.
	pub fn failed(verdict: Verdict, outcome: O, error: String) -> Self {
		Self { outcome, verdict: Some(verdict), backoff: None, error }
	}
}

/// Hides credentials and API keys of a URL when it is logged: the user info, path and query.
pub fn redact(url: &str) -> String {
	match url.find("://") {
		Some(i) => {
			let (scheme, rest) = url.split_at(i + 3);
			let host_end = rest.find(['/', '?']).unwrap_or(rest.len());
			let host = rest[..host_end].rsplit('@').next().unwrap_or_default();
			let hidden = host_end < rest.trim_end_matches('/').len();
			format!("{scheme}{host}{}", if hidden { "/…" } else { "" })
		},
		None => url.to_string(),
	}
}

/// An endpoint asking for a longer pause than this (e.g. a daily quota) is left for another.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The most a primary's probe hold grows (2^n) while it keeps being rotated away from for its
/// data (a head behind or stuck).
const MAX_DEMOTION_EXP: u32 = 4;

/// When to probe the primary.
#[derive(Debug, Default)]
struct Probe {
	/// When the primary may be probed next. `None` on the primary.
	next_at: Option<Instant>,
	/// Rotations away from the primary for its data since it last stayed in use.
	demotions: u32,
	/// When the primary was last switched back to.
	back_at: Option<Instant>,
}

/// The failover state of one chain: which endpoint is in use, and when to probe the primary.
#[derive(Debug)]
pub struct FailoverState {
	/// The chain name.
	chain_name: String,
	/// The endpoint URLs (redacted), primary first.
	urls: Vec<String>,
	/// The index of the endpoint in use.
	current: AtomicUsize,
	/// When to probe the primary.
	probe: Mutex<Probe>,
	/// How long requests stay on a fallback before the primary is tried again.
	probe_primary: Duration,
}

pub type FailoverHandle = Arc<FailoverState>;

impl FailoverState {
	/// Creates the state of `urls`, the primary first.
	pub fn new(chain_name: String, urls: &[String], probe_primary: Duration) -> FailoverHandle {
		assert!(!urls.is_empty(), "a failover needs at least one endpoint");
		Arc::new(Self {
			chain_name,
			urls: urls.iter().map(|url| redact(url)).collect(),
			current: AtomicUsize::new(0),
			probe: Mutex::new(Probe::default()),
			probe_primary,
		})
	}

	/// The chain name.
	pub fn chain_name(&self) -> &str {
		&self.chain_name
	}

	/// Whether there is any endpoint to switch to.
	pub fn has_fallbacks(&self) -> bool {
		self.urls.len() > 1
	}

	/// The URL (redacted) of the endpoint at `index`.
	pub fn url(&self, index: usize) -> &str {
		&self.urls[index]
	}

	/// The URL (redacted) of the endpoint in use.
	pub fn current_url(&self) -> &str {
		&self.urls[self.current.load(Ordering::SeqCst)]
	}

	/// Switches to the next endpoint without a failed request, for when the data itself looks
	/// wrong (e.g. a head that is behind or stuck). The primary would answer a probe just as
	/// well, so it is not probed again for `hold` (at least the probe interval), doubled each
	/// time it is rotated away from again without having stayed in use for a while.
	pub fn rotate(&self, reason: &str, hold: Duration) {
		if !self.has_fallbacks() {
			return;
		}
		let mut probe = self.probe.lock().unwrap();
		let current = self.current.load(Ordering::SeqCst);
		let next = (current + 1) % self.urls.len();
		// another caller switched away from it already
		if self
			.current
			.compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
			.is_err()
		{
			return;
		}
		if current == 0 {
			if probe.back_at.is_some_and(|at| at.elapsed() >= self.probe_primary * 2) {
				probe.demotions = 0;
			}
			probe.demotions += 1;
		}
		let hold = self.probe_primary.max(hold)
			* 2u32.pow(probe.demotions.saturating_sub(1).min(MAX_DEMOTION_EXP));
		probe.next_at = (next != 0).then(|| Instant::now() + hold);
		if next == 0 {
			probe.back_at = Some(Instant::now());
		}
		drop(probe);
		self.report_switch(current, next, reason);
	}

	/// Runs a request through the failover. `attempt(index)` makes one attempt on the endpoint
	/// at `index`; `what` names the request in logs.
	pub async fn execute<O, F, Fut>(
		&self,
		what: impl Fn() -> String,
		max_retries: u32,
		retry_interval: Duration,
		mut attempt: F,
	) -> O
	where
		F: FnMut(usize) -> Fut,
		Fut: Future<Output = Attempt<O>>,
	{
		let n = self.urls.len();
		let (observed, probing) = self.start();
		// a probe tries the primary first, then goes straight back to the endpoint in use
		let order: Vec<usize> = if probing {
			std::iter::once(0)
				.chain((0..n).map(|step| (observed + step) % n).filter(|&index| index != 0))
				.collect()
		} else {
			(0..n).map(|step| (observed + step) % n).collect()
		};

		let mut failed_over = false;
		let mut reason = String::new();
		let mut last = None;
		for (step, &index) in order.iter().enumerate() {
			// a probe is a single attempt, so a primary that is still down costs little
			let attempts = if probing && step == 0 { 1 } else { max_retries + 1 };
			let last_endpoint = step + 1 == order.len();
			let mut tries = 0;
			let (verdict, attempted) = loop {
				tries += 1;
				let attempted = attempt(index).await;
				let Some(verdict) = attempted.verdict else {
					self.served(index, observed, probing, failed_over, &reason);
					return attempted.outcome;
				};
				match verdict {
					Verdict::Fatal => {
						self.served(index, observed, probing, failed_over, &reason);
						return attempted.outcome;
					},
					Verdict::Reject if tries >= attempts => {
						self.served(index, observed, probing, failed_over, &reason);
						return attempted.outcome;
					},
					Verdict::Retry if tries >= attempts => break (verdict, attempted),
					Verdict::Rotate => break (verdict, attempted),
					// with no other endpoint left to ask, retried like any other failure: it may
					// be transient (e.g. Alchemy's "Unable to complete request")
					Verdict::Unsupported if !last_endpoint || tries >= attempts => {
						break (verdict, attempted);
					},
					Verdict::Reject | Verdict::Retry | Verdict::Unsupported => {
						let backoff = attempted.backoff.unwrap_or(retry_interval);
						if backoff > MAX_BACKOFF && self.has_fallbacks() {
							break (Verdict::Rotate, attempted);
						}
						sleep(backoff).await;
					},
				}
			};

			// the endpoint in use failed itself, rather than only lacked the method
			if index == observed && verdict != Verdict::Unsupported {
				failed_over = true;
			}
			reason = format!("{} failed: {}", what(), attempted.error);
			if !last_endpoint {
				log::warn!(
					target: &self.chain_name,
					"-[{}] ⚠️  {} on {}. Trying the next provider",
					sub_display_format(SUB_LOG_TARGET),
					reason,
					self.urls[index],
				);
			}
			// an endpoint that could not answer says less than one that failed
			if verdict != Verdict::Unsupported || last.is_none() {
				last = Some(attempted.outcome);
			}
		}
		last.expect("at least one endpoint was tried")
	}

	/// The endpoint in use when a request starts, and whether the request probes the primary.
	/// A due probe is claimed by scheduling the next one, so one request probes at a time.
	fn start(&self) -> (usize, bool) {
		let current = self.current.load(Ordering::SeqCst);
		if current == 0 {
			return (0, false);
		}
		let mut probe = self.probe.lock().unwrap();
		if probe.next_at.is_some_and(|at| Instant::now() < at) {
			return (current, false);
		}
		probe.next_at = Some(Instant::now() + self.probe_primary);
		(current, true)
	}

	/// Records that endpoint `index` served a request that started while `observed` was in use.
	/// `failed_over` is set when `observed` failed (rather than only lacked the method).
	/// Switches only from `observed`: a request that was in flight across a switch (another
	/// failover, a rotation, a probe) does not undo it.
	fn served(
		&self,
		index: usize,
		observed: usize,
		probing: bool,
		failed_over: bool,
		reason: &str,
	) {
		// served where it started, or only skipped endpoints that lacked the method
		if !failed_over && !(probing && index == 0) {
			return;
		}
		// a failed probe came back to the endpoint in use
		if index == observed {
			return;
		}
		let mut probe = self.probe.lock().unwrap();
		if self
			.current
			.compare_exchange(observed, index, Ordering::SeqCst, Ordering::SeqCst)
			.is_err()
		{
			return;
		}
		probe.next_at = (index != 0).then(|| Instant::now() + self.probe_primary);
		if index == 0 {
			probe.back_at = Some(Instant::now());
		}
		drop(probe);
		self.report_switch(observed, index, reason);
	}

	fn report_switch(&self, from: usize, to: usize, reason: &str) {
		br_metrics::set_rpc_provider_switch(&self.chain_name, to);
		if to == 0 {
			log::info!(
				target: &self.chain_name,
				"-[{}] 🔌 Back to the primary provider {} (from {})",
				sub_display_format(SUB_LOG_TARGET),
				self.urls[to],
				self.urls[from],
			);
		} else {
			br_primitives::log_and_capture_simple!(
				warn,
				"-[{}] 🔌 [{}] Switched provider {} → {}: {}",
				sub_display_format(SUB_LOG_TARGET),
				self.chain_name,
				self.urls[from],
				self.urls[to],
				reason,
			);
		}
	}
}

/// The most the stale timeout grows (2^n) while rotations do not make the head advance.
const MAX_STALE_BACKOFF_EXP: u32 = 3;

/// Watches the head a chain's endpoint reports and rotates away from one whose head is behind
/// blocks already processed, or has not advanced for the stale timeout. Such an endpoint answers
/// without errors, so the failover never leaves it by itself.
///
/// A head that stays still after a rotation too (a halted chain, one that only makes blocks on
/// demand) doubles the stale timeout each time, up to 2^`MAX_STALE_BACKOFF_EXP`, so healthy
/// endpoints are not cycled through at a steady pace.
#[derive(Debug)]
pub struct HeadMonitor {
	/// The highest head seen, and when it last advanced.
	best: (u64, Instant),
	/// How long the head may stay still, before backing off.
	stale_timeout: Duration,
	/// How far the head may trail the last processed block.
	lag_tolerance: u64,
	/// Rotations for a still head since it last advanced.
	stale_rotations: u32,
}

impl HeadMonitor {
	pub fn new(stale_timeout: Duration, lag_tolerance: u64) -> Self {
		Self { best: (0, Instant::now()), stale_timeout, lag_tolerance, stale_rotations: 0 }
	}

	/// How long the head may stay still now.
	fn stale_timeout(&self) -> Duration {
		self.stale_timeout * 2u32.pow(self.stale_rotations.min(MAX_STALE_BACKOFF_EXP))
	}

	/// Checks the `head` just reported, given the last `processed` block.
	pub fn check(&mut self, failover: &FailoverState, head: u64, processed: u64) {
		if !failover.has_fallbacks() {
			return;
		}
		if head > self.best.0 {
			self.best = (head, Instant::now());
			self.stale_rotations = 0;
		}
		let stale_for = self.best.1.elapsed();
		if head.saturating_add(self.lag_tolerance) < processed {
			failover.rotate(
				&format!("head #{head} is behind the processed block #{processed}"),
				Duration::ZERO,
			);
		} else if stale_for >= self.stale_timeout() {
			let reason = format!("head stuck at #{} for {}s", self.best.0, stale_for.as_secs());
			self.stale_rotations += 1;
			// the stuck endpoint would answer a probe fine: keep away from it as long
			failover.rotate(&reason, self.stale_timeout());
		} else {
			return;
		}
		// give the next endpoint a full window
		self.best.1 = Instant::now();
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::VecDeque;

	fn state_probing_after(n: usize, probe_primary: Duration) -> FailoverHandle {
		let urls: Vec<_> = (0..n).map(|i| format!("https://ep{i}.io")).collect();
		FailoverState::new("test".into(), &urls, probe_primary)
	}

	fn state(n: usize) -> FailoverHandle {
		state_probing_after(n, Duration::from_secs(3600))
	}

	/// Scripted endpoints: an attempt pops the next verdict of its endpoint (`None` = served,
	/// and served once the script runs out).
	struct Script {
		steps: Vec<Mutex<VecDeque<Option<Verdict>>>>,
		backoff: Vec<Option<Duration>>,
		calls: Vec<AtomicUsize>,
	}

	impl Script {
		fn new(steps: Vec<Vec<Option<Verdict>>>) -> Self {
			let n = steps.len();
			Self {
				steps: steps.into_iter().map(|s| Mutex::new(s.into())).collect(),
				backoff: vec![None; n],
				calls: (0..n).map(|_| AtomicUsize::new(0)).collect(),
			}
		}

		fn calls(&self) -> Vec<usize> {
			self.calls.iter().map(|c| c.load(Ordering::SeqCst)).collect()
		}

		/// Runs one request; returns the endpoint whose outcome it got.
		async fn run(&self, failover: &FailoverState) -> usize {
			failover
				.execute(
					|| "test".into(),
					2,
					Duration::ZERO,
					|index| {
						self.calls[index].fetch_add(1, Ordering::SeqCst);
						let verdict = self.steps[index].lock().unwrap().pop_front().flatten();
						let backoff = self.backoff[index];
						async move {
							Attempt {
								outcome: index,
								verdict,
								backoff,
								error: format!("ep{index} failed"),
							}
						}
					},
				)
				.await
		}
	}

	const RETRY: Option<Verdict> = Some(Verdict::Retry);

	#[tokio::test]
	async fn in_flight_requests_do_not_undo_a_switch() {
		// a request that started on the primary is served there after a rotation
		let failover = state(2);
		let (observed, probing) = failover.start();
		failover.rotate("head is stuck", Duration::ZERO);
		failover.served(0, observed, probing, false, "");
		assert_eq!(failover.current_url(), "https://ep1.io");

		// a request that failed over from the primary lands after a rotation elsewhere
		let failover = state(3);
		let (observed, probing) = failover.start();
		failover.rotate("head is stuck", Duration::ZERO);
		failover.served(2, observed, probing, true, "ep0 failed");
		assert_eq!(failover.current_url(), "https://ep1.io");
	}

	const UNSUPPORTED: Option<Verdict> = Some(Verdict::Unsupported);

	#[tokio::test]
	async fn unsupported_is_retried_when_no_other_endpoint_is_left() {
		// a single endpoint: a transient opaque error is retried like before the failover
		let failover = state(1);
		let script = Script::new(vec![vec![UNSUPPORTED, UNSUPPORTED]]);
		assert_eq!(script.run(&failover).await, 0);
		assert_eq!(script.calls(), vec![3]);

		// the last endpoint left is retried, the others are skipped at once
		let failover = state(2);
		let script = Script::new(vec![vec![UNSUPPORTED], vec![UNSUPPORTED; 3]]);
		script.run(&failover).await;
		assert_eq!(script.calls(), vec![1, 3]);
		assert_eq!(failover.current_url(), "https://ep0.io");
	}

	#[tokio::test]
	async fn a_healthy_endpoint_lacking_the_method_is_not_left() {
		let failover = state(3);
		let script = Script::new(vec![vec![UNSUPPORTED], vec![RETRY; 3], vec![]]);
		assert_eq!(script.run(&failover).await, 2);
		assert_eq!(failover.current_url(), "https://ep0.io");
	}

	#[test]
	fn repeated_demotions_hold_the_primary_off_longer() {
		let probe = Duration::from_millis(40);
		let failover = state_probing_after(2, probe);
		let held_for = |failover: &FailoverState| {
			failover.probe.lock().unwrap().next_at.unwrap().duration_since(Instant::now())
		};

		failover.rotate("head is behind", Duration::ZERO);
		assert!(held_for(&failover) <= probe);
		// a probe takes it back, and it is demoted again at once: twice as long
		failover.served(0, 1, true, false, "");
		failover.rotate("head is behind", Duration::ZERO);
		assert!(held_for(&failover) > probe);

		// once it stayed in use for a while, it starts over
		failover.served(0, 1, true, false, "");
		std::thread::sleep(probe * 2);
		failover.rotate("head is behind", Duration::ZERO);
		assert!(held_for(&failover) <= probe);
	}

	#[test]
	fn rotation_holds_off_the_primary_probe() {
		let failover = state_probing_after(2, Duration::ZERO);
		failover.rotate("head is stuck", Duration::from_secs(3600));
		assert_eq!(failover.start(), (1, false));

		let failover = state_probing_after(2, Duration::ZERO);
		failover.rotate("head is behind", Duration::ZERO);
		assert_eq!(failover.start(), (1, true));
	}

	#[tokio::test]
	async fn failed_probe_goes_straight_back_to_the_endpoint_in_use() {
		let failover = state_probing_after(3, Duration::ZERO);
		let script = Script::new(vec![vec![RETRY; 4], vec![RETRY; 3], vec![]]);

		assert_eq!(script.run(&failover).await, 2);
		assert_eq!(script.calls(), vec![3, 3, 1]);
		assert_eq!(failover.current_url(), "https://ep2.io");

		// the probe tries the primary once, then skips the fallback that failed too
		assert_eq!(script.run(&failover).await, 2);
		assert_eq!(script.calls(), vec![4, 3, 2]);
		assert_eq!(failover.current_url(), "https://ep2.io");
	}

	#[tokio::test]
	async fn long_backoff_requests_fail_over_at_once() {
		let failover = state(2);
		let mut script = Script::new(vec![vec![RETRY], vec![]]);
		script.backoff[0] = Some(Duration::from_secs(3600));

		let started = Instant::now();
		assert_eq!(script.run(&failover).await, 1);
		assert!(started.elapsed() < Duration::from_secs(1));
		assert_eq!(script.calls(), vec![1, 1]);
	}

	#[test]
	fn rotates_by_hand() {
		let failover = state(2);
		failover.rotate("head is stuck", Duration::ZERO);
		assert_eq!(failover.current_url(), "https://ep1.io");
		failover.rotate("head is stuck", Duration::ZERO);
		assert_eq!(failover.current_url(), "https://ep0.io");

		let single = state(1);
		single.rotate("head is stuck", Duration::ZERO);
		assert_eq!(single.current_url(), "https://ep0.io");
	}

	#[test]
	fn redacts_credentials_and_keys() {
		assert_eq!(redact("https://mainnet.infura.io/v3/SECRET"), "https://mainnet.infura.io/…");
		assert_eq!(redact("https://rpc.example.org"), "https://rpc.example.org");
		assert_eq!(redact("https://rpc.example.org/"), "https://rpc.example.org");
		assert_eq!(redact("http://user:pass@127.0.0.1:8332/"), "http://127.0.0.1:8332");
		assert_eq!(redact("https://rpc.example.org?apikey=SECRET"), "https://rpc.example.org/…");
	}

	#[test]
	fn head_behind_the_processed_block_rotates() {
		let failover = state(2);
		let mut monitor = HeadMonitor::new(Duration::from_secs(3600), 5);
		monitor.check(&failover, 100, 100);
		// within the tolerance
		monitor.check(&failover, 95, 100);
		assert_eq!(failover.current_url(), "https://ep0.io");
		monitor.check(&failover, 94, 100);
		assert_eq!(failover.current_url(), "https://ep1.io");
	}

	#[test]
	fn stuck_head_rotates() {
		let failover = state(2);
		let mut monitor = HeadMonitor::new(Duration::ZERO, 0);
		monitor.check(&failover, 100, 90);
		assert_eq!(failover.current_url(), "https://ep1.io");
	}

	#[test]
	fn advancing_head_does_not_rotate() {
		let failover = state(2);
		let mut monitor = HeadMonitor::new(Duration::from_secs(3600), 0);
		for head in 100..110 {
			monitor.check(&failover, head, head - 2);
		}
		assert_eq!(failover.current_url(), "https://ep0.io");
	}

	#[test]
	fn still_head_backs_off_after_each_rotation() {
		let failover = state(2);
		let mut monitor = HeadMonitor::new(Duration::from_millis(50), 0);
		monitor.check(&failover, 100, 90);

		std::thread::sleep(Duration::from_millis(60));
		monitor.check(&failover, 100, 90);
		assert_eq!(failover.current_url(), "https://ep1.io");

		// the next endpoint has the same head: now it may stay still for 100ms
		std::thread::sleep(Duration::from_millis(60));
		monitor.check(&failover, 100, 90);
		assert_eq!(failover.current_url(), "https://ep1.io");
		std::thread::sleep(Duration::from_millis(60));
		monitor.check(&failover, 100, 90);
		assert_eq!(failover.current_url(), "https://ep0.io");

		// an advancing head resets it
		monitor.check(&failover, 101, 90);
		assert_eq!(monitor.stale_timeout(), Duration::from_millis(50));
	}

	#[test]
	fn single_endpoint_never_rotates() {
		let failover = state(1);
		let mut monitor = HeadMonitor::new(Duration::ZERO, 0);
		monitor.check(&failover, 1, 100);
		assert_eq!(failover.current_url(), "https://ep0.io");
	}
}

/// A local HTTP node for the transports' tests.
#[cfg(test)]
pub(crate) mod test_node {
	use reqwest::Url;
	use std::sync::{Arc, Mutex};
	use tokio::{
		io::{AsyncReadExt, AsyncWriteExt},
		net::TcpListener,
	};

	/// A node that answers each request with the next scripted `(status, body)`, and records
	/// the request lines it got.
	pub async fn node(script: Vec<(u16, &'static str)>) -> (Url, Arc<Mutex<Vec<String>>>) {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
		let requests = Arc::new(Mutex::new(vec![]));
		let seen = requests.clone();
		tokio::spawn(async move {
			for (status, body) in script {
				let (mut stream, _) = listener.accept().await.unwrap();
				let mut buf = vec![0u8; 16 * 1024];
				let mut len = 0;
				// read the headers, then the body by its content-length
				loop {
					len += stream.read(&mut buf[len..]).await.unwrap();
					let text = String::from_utf8_lossy(&buf[..len]).to_string();
					if let Some(end) = text.find("\r\n\r\n") {
						let content_length = text
							.lines()
							.find_map(|l| {
								l.to_lowercase()
									.strip_prefix("content-length:")
									.map(|v| v.trim().parse::<usize>().unwrap())
							})
							.unwrap_or(0);
						if len >= end + 4 + content_length {
							seen.lock().unwrap().push(
								text.lines().next().unwrap().to_string() + " " + &text[end + 4..],
							);
							break;
						}
					}
				}
				let response = format!(
					"HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
					body.len()
				);
				stream.write_all(response.as_bytes()).await.unwrap();
			}
		});
		(url, requests)
	}

	/// The URL of a local port nothing listens on.
	pub async fn closed_port() -> Url {
		let addr = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
		Url::parse(&format!("http://{addr}/")).unwrap()
	}
}
