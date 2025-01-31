use super::*;

/// The relayer client dependencies.
pub struct FullDeps<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	pub manager_deps: ManagerDeps<F, P, T>,
	pub periodic_deps: PeriodicDeps<F, P, T>,
	pub handler_deps: HandlerDeps<F, P, T>,
	pub substrate_deps: SubstrateDeps<F, P, T>,
	pub btc_deps: BtcDeps<F, P, T>,
}
