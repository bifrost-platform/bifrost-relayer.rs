use prometheus::Opts;
use prometheus_endpoint::{GaugeVec, Registry, U64};

/// Register EVM chain related prometheus metrics.
pub fn register_evm_prometheus_metrics(registry: &Registry) {
	registry.register(Box::new(BLOCK_HEIGHT.clone())).unwrap();
	registry.register(Box::new(RPC_CALLS.clone())).unwrap();
	registry.register(Box::new(PAYED_FEES.clone())).unwrap();
	registry.register(Box::new(RELAYED_TRANSACTIONS.clone())).unwrap();
}

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
	pub static ref PAYED_FEES: GaugeVec<U64> = GaugeVec::<U64>::new(
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
