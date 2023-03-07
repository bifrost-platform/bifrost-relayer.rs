use ethers::types::TransactionRequest;

use crate::SocketEvent;

pub(crate) trait TransactionManager {
	fn build(
		&self,
		_contract_name: &str,
		_method_name: &str,
		socket_event: SocketEvent,
	) -> TransactionRequest;
	fn send(&self);
}
