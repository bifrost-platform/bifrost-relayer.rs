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
}

/// Register prometheus metrics and setup initial values.
pub fn setup(registry: &Registry) {
	register_system_prometheus_metrics(registry);
	register_evm_prometheus_metrics(registry);
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

/// Set the relayer uptime.
fn set_system_uptime() {
	let start_time_since_epoch =
		SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
	PROCESS_UPTIME.set(start_time_since_epoch.as_secs());
}
