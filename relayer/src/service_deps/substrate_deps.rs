use super::*;

pub struct SubstrateDeps<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// The `UnsignedTransactionManager` for Bifrost.
	pub unsigned_tx_manager: UnsignedTransactionManager<F, P, T>,
	/// The `XtRequestSender` for Bifrost.
	pub xt_request_sender: Arc<XtRequestSender>,
}

impl<F, P, T> SubstrateDeps<F, P, T>
where
	F: TxFiller + WalletProvider + 'static,
	P: Provider<T> + 'static,
	T: Transport + Clone,
{
	pub fn new(bfc_client: Arc<EthClient<F, P, T>>, task_manager: &TaskManager) -> Self {
		let (unsigned_tx_manager, sender) =
			UnsignedTransactionManager::new(bfc_client.clone(), task_manager.spawn_handle());

		let xt_request_sender = Arc::new(XtRequestSender::new(sender));

		Self { unsigned_tx_manager, xt_request_sender }
	}
}
