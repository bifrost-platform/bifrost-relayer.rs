use super::*;
use br_primitives::substrate::{CustomConfig, initialize_sub_client};
use subxt::OnlineClient;

pub struct SubstrateDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The `UnsignedTransactionManager` for Bifrost.
	pub unsigned_tx_manager: UnsignedTransactionManager<F, P, N>,
	/// The `XtRequestSender` for Bifrost.
	pub xt_request_sender: Arc<XtRequestSender>,
	/// The Substrate client for storage queries and event subscription.
	pub sub_client: OnlineClient<CustomConfig>,
	/// The Substrate RPC URL for legacy RPC methods.
	pub sub_rpc_url: String,
}

impl<F, P, N: AlloyNetwork> SubstrateDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	pub async fn new(bfc_client: Arc<EthClient<F, P, N>>, task_manager: &TaskManager) -> Self {
		let (unsigned_tx_manager, sender) =
			UnsignedTransactionManager::new(bfc_client.clone(), task_manager.spawn_handle());

		let xt_request_sender = Arc::new(XtRequestSender::new(sender));

		// Initialize Substrate client
		let sub_rpc_url = bfc_client.get_url().to_string();
		let sub_client = initialize_sub_client(bfc_client.get_url()).await;

		Self { unsigned_tx_manager, xt_request_sender, sub_client, sub_rpc_url }
	}
}
