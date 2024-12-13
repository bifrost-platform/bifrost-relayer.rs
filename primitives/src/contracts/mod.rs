pub mod authority;
pub mod bitcoin_socket;
pub mod chainlink_aggregator;
pub mod registration_pool;
pub mod relay_executive;
pub mod relayer_manager;
pub mod socket;
pub mod socket_queue;

use alloy::{
	network::AnyNetwork,
	primitives::{b256, Bytes, PrimitiveSignature, B256},
	providers::fillers::FillProvider,
	sol,
};
use std::{collections::BTreeMap, sync::Arc};
