use super::*;

pub struct SubstrateDeps<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// The `UnsignedTransactionManager` for Bifrost.
	pub unsigned_tx_manager: UnsignedTransactionManager<F, P>,
	/// The `XtRequestSender` for Bifrost.
	pub xt_request_sender: Arc<XtRequestSender>,
}

impl<F, P> SubstrateDeps<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<AnyNetwork> + 'static,
{
	pub fn new(bfc_client: Arc<EthClient<F, P>>, task_manager: &TaskManager) -> Self {
		let (unsigned_tx_manager, sender) =
			UnsignedTransactionManager::new(bfc_client.clone(), task_manager.spawn_handle());

		let xt_request_sender = Arc::new(XtRequestSender::new(sender));

		Self { unsigned_tx_manager, xt_request_sender }
	}
}
