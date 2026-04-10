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
	pub btc_deps: BtcDeps<F, P, N>,
	/// Per-cluster Solana wiring. Empty if no `sol_providers` are
	/// configured. Each entry owns its own SlotManager + inbound +
	/// outbound handler trio (mirror of `BtcDeps`, but per-cluster
	/// because the relayer can observe multiple Solana networks).
	pub sol_deps: Vec<SolDeps<F, P, N>>,
}
