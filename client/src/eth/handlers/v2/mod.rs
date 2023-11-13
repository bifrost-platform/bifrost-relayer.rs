use br_primitives::{eth::SocketVariants, socket::SocketMessage};
use ethers::types::{Bytes, Log};

pub use execution_filter::*;

mod execution_filter;

#[async_trait::async_trait]
pub trait V2Handler {
	/// Decode and parse the event if the given log triggered an relay target event.
	async fn process_confirmed_log(&self, log: &Log, is_bootstrap: bool);

	/// Verifies whether the given transaction interacted with the target contract.
	fn is_target_contract(&self, log: &Log) -> bool;

	// Verifies whether the emitted `Socket` event is based on V2. We consider V2 if `socket.msg.params.variants` field exists.
	fn is_version2(&self, msg: &SocketMessage) -> bool;

	/// Decodes the `socket.msg.params.variants` field into `(sender, gas_limit, data)`.
	fn decode_msg_variants(&self, variants: &Bytes) -> SocketVariants;
}
