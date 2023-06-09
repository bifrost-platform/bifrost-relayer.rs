use prometheus::{
	HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
};

lazy_static! {
	// TODO: 체인별로 메트릭 구분?
	pub static ref BRIDGE_RELAY_TRANSACTIONS: IntCounter =
		IntCounter::new("bridge_relay_transactions", "Bridge Relay Transactions").unwrap();
	pub static ref ROUNDUP_RELAY_TRANSACTIONS: IntCounter =
		IntCounter::new("roundup_relay_transactions", "RoundUp Relay Transactions").unwrap();
	pub static ref PRICE_FEED_TRANSACTIONS: IntCounter =
		IntCounter::new("price_feed_transactions", "Price Feed Transactions").unwrap();
	pub static ref PAYED_TRANSACTION_FEES: IntCounter =
		IntCounter::new("payed_transaction_fees", "Payed Transaction Fees").unwrap();
}

pub fn register_metrics(registry: &Registry) {
	registry.register(Box::new(BRIDGE_RELAY_TRANSACTIONS.clone())).unwrap();
	registry.register(Box::new(ROUNDUP_RELAY_TRANSACTIONS.clone())).unwrap();
	registry.register(Box::new(PRICE_FEED_TRANSACTIONS.clone())).unwrap();
	registry.register(Box::new(PAYED_TRANSACTION_FEES.clone())).unwrap();
}

// TODO: init_prometheus from substrate
