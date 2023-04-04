use cccp_client::eth::EthClient;
use cron::Schedule;
use std::sync::Arc;

pub struct RoundupEmitter<T> {
	/// Current round number
	pub current_round: u64,
	/// The ethereum client for the Bifrost network.
	pub client: Arc<EthClient<T>>,
	/// The time schedule that represents when to check round info.
	pub schedule: Schedule,
}
