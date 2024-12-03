use super::*;

pub struct PeriodicDeps<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// The `HeartbeatSender` used for system health checks.
	pub heartbeat_sender: HeartbeatSender<F, P, T>,
	/// The `OraclePriceFeeder` used for price feeding.
	pub oracle_price_feeder: OraclePriceFeeder<F, P, T>,
	/// The `RoundupEmitter` used for detecting and emitting new round updates.
	pub roundup_emitter: RoundupEmitter<F, P, T>,
	/// The `KeypairMigrator` used for detecting migration sequences.
	pub keypair_migrator: KeypairMigrator<F, P, T>,
	/// The `PubKeyPreSubmitter` used for presubmitting public keys.
	pub presubmitter: PubKeyPreSubmitter<F, P, T>,
}

impl<F, P, T> PeriodicDeps<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	pub fn new(
		bootstrap_shared_data: BootstrapSharedData,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		keypair_storage: Arc<RwLock<KeypairStorage>>,
		substrate_deps: &SubstrateDeps<F, P, T>,
		clients: Arc<BTreeMap<ChainId, Arc<EthClient<F, P, T>>>>,
		bfc_client: Arc<EthClient<F, P, T>>,
	) -> Self {
		// initialize the heartbeat sender
		let heartbeat_sender = HeartbeatSender::new(bfc_client.clone(), clients.clone());

		// initialize the oracle price feeder
		let oracle_price_feeder = OraclePriceFeeder::new(bfc_client.clone(), clients.clone());

		// initialize the roundup emitter
		let roundup_emitter = RoundupEmitter::new(
			bfc_client.clone(),
			clients.clone(),
			Arc::new(bootstrap_shared_data.clone()),
		);

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
			keypair_migrator,
			presubmitter,
		}
	}
}
