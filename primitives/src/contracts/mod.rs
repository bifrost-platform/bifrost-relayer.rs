pub mod authority;
pub mod bitcoin_socket;
pub mod blaze;
pub mod chainlink_aggregator;
pub mod hooks;
pub mod oracle;
pub mod registration_pool;
pub mod relay_executive;
pub mod relayer_manager;
pub mod socket;
pub mod socket_queue;

use alloy::{
	primitives::{Address, B256, Bytes, Signature, U256, b256},
	providers::fillers::FillProvider,
	sol,
};
use std::{collections::BTreeMap, sync::Arc};
