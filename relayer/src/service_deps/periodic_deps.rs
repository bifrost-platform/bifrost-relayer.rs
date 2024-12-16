use br_client::eth::ClientMap;

use super::*;

pub struct PeriodicDeps<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
{
	/// The `HeartbeatSender` used for system health checks.
	pub heartbeat_sender: HeartbeatSender<F, P, T>,
	/// The `OraclePriceFeeder` used for price feeding.
	pub oracle_price_feeder: OraclePriceFeeder<F, P, T>,
	/// The `RoundupEmitter` used for detecting and emitting new round updates.
	pub roundup_emitter: RoundupEmitter<F, P, T>,
	/// The `SocketRollbackEmitter`'s for each specified chain.
	pub rollback_emitters: Vec<SocketRollbackEmitter<F, P, T>>,
	/// The `RollbackSender`'s for each specified chain.
	pub rollback_senders: Arc<BTreeMap<ChainId, Arc<UnboundedSender<Socket_Message>>>>,
	/// The `KeypairMigrator` used for detecting migration sequences.
	pub keypair_migrator: KeypairMigrator<F, P, T>,
	/// The `PubKeyPreSubmitter` used for presubmitting public keys.
	pub presubmitter: PubKeyPreSubmitter<F, P, T>,
}

impl<F, P, T> PeriodicDeps<F, P, T>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
{
	pub fn new(
		bootstrap_shared_data: BootstrapSharedData,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		keypair_storage: Arc<RwLock<KeypairStorage>>,
		substrate_deps: &SubstrateDeps<F, P, T>,
		clients: Arc<ClientMap<F, P, T>>,
		bfc_client: Arc<EthClient<F, P, T>>,
		task_manager: &TaskManager,
		debug_mode: bool,
	) -> Self {
		// initialize the heartbeat sender
		let heartbeat_sender =
			HeartbeatSender::new(bfc_client.clone(), task_manager.spawn_handle(), debug_mode);

		// initialize the oracle price feeder
		let oracle_price_feeder = OraclePriceFeeder::new(
			bfc_client.clone(),
			clients.clone(),
			task_manager.spawn_handle(),
			debug_mode,
		);

		// initialize the roundup emitter
		let roundup_emitter = RoundupEmitter::new(
			bfc_client.clone(),
			Arc::new(bootstrap_shared_data.clone()),
			task_manager.spawn_handle(),
			debug_mode,
		);

		let mut rollback_emitters = vec![];
		let mut rollback_senders = BTreeMap::new();
		clients.iter().for_each(|(chain_id, client)| {
			let (rollback_emitter, rollback_sender) = SocketRollbackEmitter::new(
				client.clone(),
				clients.clone(),
				task_manager.spawn_handle(),
				debug_mode,
			);
			rollback_emitters.push(rollback_emitter);
			rollback_senders.insert(*chain_id, rollback_sender);
		});

		// initialize migration detector
		let keypair_migrator = KeypairMigrator::new(
			bfc_client.clone(),
			migration_sequence.clone(),
			keypair_storage.clone(),
		);
		let presubmitter = PubKeyPreSubmitter::new(
			bfc_client.clone(),
			substrate_deps.xt_request_sender.clone(),
			keypair_storage.clone(),
			migration_sequence.clone(),
		);

		Self {
			heartbeat_sender,
			oracle_price_feeder,
			roundup_emitter,
			rollback_emitters,
			rollback_senders: Arc::new(rollback_senders),
			keypair_migrator,
			presubmitter,
		}
	}
}
