use super::*;

/// The relayer client dependencies.
pub struct FullDeps<F, P, N: AlloyNetwork = AnyNetwork>
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	pub manager_deps: ManagerDeps<F, P, N>,
	pub periodic_deps: PeriodicDeps<F, P, N>,
	pub handler_deps: HandlerDeps<F, P, N>,
	pub substrate_deps: SubstrateDeps<F, P, N>,
	/// Bitcoin wiring. `None` when the runtime has no Bitcoin pallets.
	pub btc_deps: Option<BtcDeps<F, P, N>>,
	/// Solana wiring. `None` when `sol_provider` is not configured.
	pub sol_deps: Option<SolDeps<F, P, N>>,
}
