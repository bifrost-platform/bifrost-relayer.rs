use ethers::{types::TransactionReceipt, utils::WEI_IN_ETHER};
use prometheus::Opts;
use prometheus_endpoint::{GaugeVec, Registry, F64, U64};

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
		Opts::new("relayer_payed_fees", "Payed transaction fees of the chain"),
		&["chain_name"],
	)
	.unwrap();
	pub static ref RELAYED_TRANSACTIONS: GaugeVec<U64> = GaugeVec::<U64>::new(
		Opts::new("relayer_relayed_transactions", "Relayed transactions of the chain"),
		&["chain_name"],
	)
	.unwrap();
}

/// Register EVM chain related prometheus metrics.
pub fn register_evm_prometheus_metrics(registry: &Registry) {
	registry.register(Box::new(BLOCK_HEIGHT.clone())).unwrap();
	registry.register(Box::new(RPC_CALLS.clone())).unwrap();
	registry.register(Box::new(NATIVE_BALANCE.clone())).unwrap();
	registry.register(Box::new(PAYED_FEES.clone())).unwrap();
	registry.register(Box::new(RELAYED_TRANSACTIONS.clone())).unwrap();
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
	let payed_fee = (receipt.effective_gas_price.unwrap().as_u64() as f64 *
		receipt.gas_used.unwrap().as_u64() as f64) /
		WEI_IN_ETHER.as_u64() as f64;
	PAYED_FEES
		.with_label_values(&[label])
		.set(PAYED_FEES.with_label_values(&[label]).get() + payed_fee)
}

/// Increase the relayed transaction counter.
pub fn increase_relayed_transactions(label: &str) {
	RELAYED_TRANSACTIONS.with_label_values(&[label]).inc();
}
