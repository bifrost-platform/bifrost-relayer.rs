use super::*;

/// The relayer client dependencies.
pub struct FullDeps<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<AnyNetwork> + 'static,
{
	pub manager_deps: ManagerDeps<F, P>,
	pub periodic_deps: PeriodicDeps<F, P>,
	pub handler_deps: HandlerDeps<F, P>,
	pub substrate_deps: SubstrateDeps<F, P>,
	pub btc_deps: BtcDeps<F, P>,
}
