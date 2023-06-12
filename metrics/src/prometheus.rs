use prometheus::{Gauge, Registry};

pub fn register_evm_prometheus_metrics(
	metrics: &Option<EvmPrometheusMetrics>,
	registry: &Registry,
) {
	if let Some(metrics) = metrics {
		registry.register(Box::new(metrics.block_height.clone())).unwrap();
		registry.register(Box::new(metrics.rpc_calls.clone())).unwrap();
		registry.register(Box::new(metrics.payed_fees.clone())).unwrap();
		registry.register(Box::new(metrics.relayed_transactions.clone())).unwrap();
	}
}

pub struct EvmPrometheusMetrics {
	/// The highest processed block.
	pub block_height: Gauge,
	/// The JSON RPC call count.
	pub rpc_calls: Gauge,
	/// The transaction fees payed.
	pub payed_fees: Gauge,
	/// The relayed transaction count.
	pub relayed_transactions: Gauge,
}

impl EvmPrometheusMetrics {
	pub fn new(chain_name: String) -> Self {
		Self {
			block_height: Gauge::new(
				format!("{}_block_height", chain_name.to_lowercase()),
				format!("Block Height [{}]", chain_name.to_uppercase()),
			)
			.unwrap(),
			rpc_calls: Gauge::new(
				format!("{}_rpc_calls", chain_name.to_lowercase()),
				format!("RPC Calls [{}]", chain_name.to_uppercase()),
			)
			.unwrap(),
			payed_fees: Gauge::new(
				format!("{}_payed_fees", chain_name.to_lowercase()),
				format!("Payed Fees [{}]", chain_name.to_uppercase()),
			)
			.unwrap(),
			relayed_transactions: Gauge::new(
				format!("{}_relayed_transactions", chain_name.to_lowercase()),
				format!("Relayed Transactions [{}]", chain_name.to_uppercase()),
			)
			.unwrap(),
		}
	}
}
