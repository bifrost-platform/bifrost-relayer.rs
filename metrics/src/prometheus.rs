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

	// ----------------------------------------------------------------
	// Solana / cccp-solana metrics
	// ----------------------------------------------------------------

	/// Latest slot the slot manager has observed for a given cluster.
	pub static ref SOL_SLOT_HEIGHT: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new("relayer_sol_slot_height", "Latest observed slot per Solana cluster"),
		&["cluster"],
	)
	.unwrap();

	/// Cumulative count of `getSlot`/`getSignaturesForAddress`/`getTransaction`
	/// RPC requests issued to a given cluster.
	pub static ref SOL_RPC_CALLS: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new("relayer_sol_rpc_calls", "Cumulative Solana RPC call count per cluster"),
		&["cluster"],
	)
	.unwrap();

	/// Cumulative count of inbound CCCP events the slot manager has
	/// decoded and broadcast to handlers.
	pub static ref SOL_INBOUND_EVENTS: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_inbound_events",
			"Cumulative count of inbound SocketEvents decoded from a Solana cluster",
		),
		&["cluster"],
	)
	.unwrap();

	/// Cumulative count of outbound `poll(...)` / `round_control_relay(...)`
	/// transactions submitted to a given cluster.
	pub static ref SOL_OUTBOUND_SUBMITTED: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_outbound_submitted",
			"Cumulative count of cccp-solana IXs submitted to a cluster",
		),
		&["cluster"],
	)
	.unwrap();

	/// Cumulative count of failed outbound submissions (after retries).
	pub static ref SOL_OUTBOUND_FAILED: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_outbound_failed",
			"Cumulative count of cccp-solana IX submissions that errored",
		),
		&["cluster"],
	)
	.unwrap();

	/// Cumulative count of outbound transactions rejected due to exceeding
	/// Solana's 1232-byte packet limit (typically too many CCCP signatures).
	pub static ref SOL_OUTBOUND_OVERSIZED: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new(
			"relayer_sol_outbound_oversized",
			"Count of outbound txs rejected for exceeding 1232B size limit",
		),
		&["cluster"],
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

/// Register Solana cluster related prometheus metrics.
fn register_sol_prometheus_metrics(registry: &Registry) {
	registry.register(Box::new(SOL_SLOT_HEIGHT.clone())).unwrap();
	registry.register(Box::new(SOL_RPC_CALLS.clone())).unwrap();
	registry.register(Box::new(SOL_INBOUND_EVENTS.clone())).unwrap();
	registry.register(Box::new(SOL_OUTBOUND_SUBMITTED.clone())).unwrap();
	registry.register(Box::new(SOL_OUTBOUND_FAILED.clone())).unwrap();
	registry.register(Box::new(SOL_OUTBOUND_OVERSIZED.clone())).unwrap();
}

/// Set the latest observed Solana slot for a given cluster.
pub fn set_sol_slot_height(cluster: &str, slot: u64) {
	SOL_SLOT_HEIGHT.with_label_values(&[cluster]).set(slot);
}

/// Increase the Solana RPC call counter for a cluster.
pub fn increase_sol_rpc_calls(cluster: &str) {
	SOL_RPC_CALLS.with_label_values(&[cluster]).inc();
}

/// Increase the Solana inbound event counter for a cluster.
pub fn add_sol_inbound_events(cluster: &str, count: u64) {
	SOL_INBOUND_EVENTS.with_label_values(&[cluster]).add(count);
}

/// Increase the Solana outbound success counter for a cluster.
pub fn increase_sol_outbound_submitted(cluster: &str) {
	SOL_OUTBOUND_SUBMITTED.with_label_values(&[cluster]).inc();
}

/// Increase the Solana outbound failure counter for a cluster.
pub fn increase_sol_outbound_failed(cluster: &str) {
	SOL_OUTBOUND_FAILED.with_label_values(&[cluster]).inc();
}

/// Increase the Solana outbound oversized-transaction counter.
pub fn increase_sol_outbound_oversized(cluster: &str) {
	SOL_OUTBOUND_OVERSIZED.with_label_values(&[cluster]).inc();
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

/// Set the relayer uptime.
fn set_system_uptime() {
	let start_time_since_epoch =
		SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
	PROCESS_UPTIME.set(start_time_since_epoch.as_secs());
}
