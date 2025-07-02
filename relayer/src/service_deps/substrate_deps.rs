use super::*;

pub struct SubstrateDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The `UnsignedTransactionManager` for Bifrost.
	pub unsigned_tx_manager: UnsignedTransactionManager<F, P, N>,
	/// The `XtRequestSender` for Bifrost.
	pub xt_request_sender: Arc<XtRequestSender>,
}

impl<F, P, N: AlloyNetwork> SubstrateDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	pub fn new(bfc_client: Arc<EthClient<F, P, N>>, task_manager: &TaskManager) -> Self {
		let (unsigned_tx_manager, sender) =
			UnsignedTransactionManager::new(bfc_client.clone(), task_manager.spawn_handle());

		let xt_request_sender = Arc::new(XtRequestSender::new(sender));

		Self { unsigned_tx_manager, xt_request_sender }
	}
}
