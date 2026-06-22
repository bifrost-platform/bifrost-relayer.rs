use std::time::SystemTime;

use alloy::{primitives::utils::format_units, rpc::types::TransactionReceipt};
use prometheus_endpoint::{F64, Gauge, GaugeVec, Opts, Registry, U64};

lazy_static! {
	pub static ref BLOCK_HEIGHT: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new("relayer_block_height", "Block height of the chain"),
		&["chain_name"],
	)
	.unwrap();
	pub static ref RPC_CALLS: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new("relayer_rpc_calls", "Requested RPC calls of the chain"),
		&["chain_name"],
	)
	.unwrap();
	pub static ref NATIVE_BALANCE: GaugeVec<F64> = GaugeVec::<F64>::new(
		Opts::new("relayer_native_balance", "Native token balance remained of the chain"),
		&["chain_name"],
	)
	.unwrap();
	pub static ref PAYED_FEES: GaugeVec<F64> = GaugeVec::<F64>::new(
		Opts::new("relayer_payed_fees", "Payed transaction fees of the chain since start"),
		&["chain_name"],
	)
	.unwrap();
	pub static ref PROCESS_UPTIME: Gauge<U64> = Gauge::<U64>::new(
		"relayer_process_start_time_seconds",
		"Number of seconds between the UNIX epoch and the moment the process started",
	)
	.unwrap();

	// -----------------------------------------------------------------
	// Solana-track metrics. Shape mirrors the EVM-track ones so the
	// existing Grafana dashboards can reuse their per-chain panels by
	// just adding the Solana cluster names to the `chain_name` label.
	// -----------------------------------------------------------------

	/// Count of Solana events the slot manager has decoded and broadcast,
	/// segmented by `type` ∈ {"inbound", "outbound", "new_slot"}. Rising
	/// `new_slot` with flat `inbound`/`outbound` indicates a live cluster
	/// with no cccp traffic; flat `new_slot` is the clear "slot manager
	/// stalled" signal.
	pub static ref SOL_EVENTS_TOTAL: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new("relayer_sol_events_total", "Solana slot manager events seen"),
		&["chain_name", "type"],
	)
	.unwrap();

	/// Count of Solana `poll` / `poll_buffered` IX submissions by
	/// outcome. Labels: `outcome` ∈ {"sent", "confirmed", "failed",
	/// "dropped"}. Paired with `SOL_PENDING_RELAYS` this gives the full
	/// delivery picture: queue depth + per-attempt outcome histogram.
	pub static ref SOL_POLL_SUBMISSIONS_TOTAL: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_poll_submissions_total",
			"Solana poll/poll_buffered IX submission outcomes",
		),
		&["chain_name", "outcome"],
	)
	.unwrap();

	/// Current depth of the per-cluster pending queue. Gauge (not
	/// counter) because the outbound handler pops entries as Bifrost
	/// catches up. Non-zero for long stretches means either RPC
	/// unavailability or a stalled BFC → Solana roundup.
	pub static ref SOL_PENDING_RELAYS: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_pending_relays",
			"Pending Solana outbound jobs awaiting round catch-up or retry",
		),
		&["chain_name"],
	)
	.unwrap();

	/// Count of `cccp-relay-queue::on_flight_poll` extrinsics this
	/// relayer has submitted for Solana-sourced CCCP traffic. Labels:
	/// `outcome` ∈ {"submitted", "skipped"}. The "skipped" bucket
	/// covers already-voted and already-finalized transfers that the
	/// queue poller short-circuited.
	pub static ref SOL_ONFLIGHT_VOTES_TOTAL: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_onflight_votes_total",
			"on_flight_poll extrinsics submitted for Solana CCCP events",
		),
		&["chain_name", "outcome"],
	)
	.unwrap();

	/// Count of `cccp-relay-queue::finalize_poll` extrinsics this
	/// relayer has submitted for Solana-sourced CCCP traffic. Same
	/// label shape as `SOL_ONFLIGHT_VOTES_TOTAL`.
	pub static ref SOL_FINALIZE_VOTES_TOTAL: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_finalize_votes_total",
			"finalize_poll extrinsics submitted for Solana CCCP events",
		),
		&["chain_name", "outcome"],
	)
	.unwrap();
}

/// Register prometheus metrics and setup initial values.
pub fn setup(registry: &Registry) {
	register_system_prometheus_metrics(registry);
	register_evm_prometheus_metrics(registry);
	register_sol_prometheus_metrics(registry);
	set_system_uptime();
}

/// Register system related prometheus metrics.
fn register_system_prometheus_metrics(registry: &Registry) {
	registry.register(Box::new(PROCESS_UPTIME.clone())).unwrap();
}

/// Register EVM chain related prometheus metrics.
fn register_evm_prometheus_metrics(registry: &Registry) {
	registry.register(Box::new(BLOCK_HEIGHT.clone())).unwrap();
	registry.register(Box::new(RPC_CALLS.clone())).unwrap();
	registry.register(Box::new(NATIVE_BALANCE.clone())).unwrap();
	registry.register(Box::new(PAYED_FEES.clone())).unwrap();
}

/// Set the block height of the chain.
pub fn set_block_height(label: &str, block_height: u64) {
	BLOCK_HEIGHT.with_label_values(&[label]).set(block_height);
}

/// Increase the RPC call counter.
pub fn increase_rpc_calls(label: &str) {
	RPC_CALLS.with_label_values(&[label]).inc();
}

/// Set the native token balance remained of the chain.
pub fn set_native_balance(label: &str, balance: f64) {
	NATIVE_BALANCE.with_label_values(&[label]).set(balance);
}

/// Increase the payed transaction fees.
pub fn set_payed_fees(label: &str, receipt: &TransactionReceipt) {
	let gas_price = receipt.effective_gas_price;
	let gas_used = receipt.gas_used;

	let payed_fee = format_units(gas_price.saturating_mul(gas_used.into()), "eth")
		.unwrap()
		.parse::<f64>()
		.unwrap();
	PAYED_FEES
		.with_label_values(&[label])
		.set(PAYED_FEES.with_label_values(&[label]).get() + payed_fee)
}

/// Register Solana chain related prometheus metrics.
fn register_sol_prometheus_metrics(registry: &Registry) {
	registry.register(Box::new(SOL_EVENTS_TOTAL.clone())).unwrap();
	registry.register(Box::new(SOL_POLL_SUBMISSIONS_TOTAL.clone())).unwrap();
	registry.register(Box::new(SOL_PENDING_RELAYS.clone())).unwrap();
	registry.register(Box::new(SOL_ONFLIGHT_VOTES_TOTAL.clone())).unwrap();
	registry.register(Box::new(SOL_FINALIZE_VOTES_TOTAL.clone())).unwrap();
}

/// Increment the Solana slot-manager event counter for `chain_name`
/// with the given `event_type` label. Use `"inbound"`, `"outbound"`,
/// or `"new_slot"`.
pub fn increase_sol_events(label: &str, event_type: &str) {
	SOL_EVENTS_TOTAL.with_label_values(&[label, event_type]).inc();
}

/// Increment the Solana poll submission counter for `chain_name` with
/// the given `outcome` label. Use `"sent"`, `"confirmed"`, `"failed"`,
/// or `"dropped"`.
pub fn increase_sol_poll_submissions(label: &str, outcome: &str) {
	SOL_POLL_SUBMISSIONS_TOTAL.with_label_values(&[label, outcome]).inc();
}

/// Set the current pending-relay queue depth for `chain_name`.
pub fn set_sol_pending_relays(label: &str, depth: u64) {
	SOL_PENDING_RELAYS.with_label_values(&[label]).set(depth);
}

/// Increment the Solana on-flight-poll vote counter. Use
/// `outcome` ∈ {"submitted", "skipped"}.
pub fn increase_sol_onflight_votes(label: &str, outcome: &str) {
	SOL_ONFLIGHT_VOTES_TOTAL.with_label_values(&[label, outcome]).inc();
}

/// Increment the Solana finalize-poll vote counter. Same label shape
/// as `increase_sol_onflight_votes`.
pub fn increase_sol_finalize_votes(label: &str, outcome: &str) {
	SOL_FINALIZE_VOTES_TOTAL.with_label_values(&[label, outcome]).inc();
}

/// Set the relayer uptime.
fn set_system_uptime() {
	let start_time_since_epoch =
		SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
	PROCESS_UPTIME.set(start_time_since_epoch.as_secs());
}
