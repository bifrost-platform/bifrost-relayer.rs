use super::*;

pub struct BtcDeps<F, P, T, K>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
	K: KeypairAccessor + 'static,
{
	/// The Bitcoin outbound handler.
	pub outbound: OutboundHandler<F, P, T>,
	/// The Bitcoin inbound handler.
	pub inbound: InboundHandler<F, P, T>,
	/// The Bitcoin block manager.
	pub block_manager: BlockManager<F, P, T>,
	/// The Bitcoin PSBT signer.
	pub psbt_signer: PsbtSigner<F, P, T, K>,
	/// The Bitcoin vault public key submitter.
	pub pub_key_submitter: PubKeySubmitter<F, P, T, K>,
	/// The Bitcoin rollback verifier.
	pub rollback_verifier: BitcoinRollbackVerifier<F, P, T>,
}

impl<F, P, T, K> BtcDeps<F, P, T, K>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
	T: Transport + Clone,
	K: KeypairAccessor + 'static,
{
	pub fn new(
		config: &Configuration,
		keypair_storage: Arc<RwLock<KeypairStorage<K>>>,
		bootstrap_shared_data: BootstrapSharedData,
		substrate_deps: &SubstrateDeps<F, P, T>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		bfc_client: Arc<EthClient<F, P, T>>,
		task_manager: &TaskManager,
		debug_mode: bool,
	) -> Self {
		let bootstrap_shared_data = Arc::new(bootstrap_shared_data.clone());
		let network = Network::from_core_arg(&config.relayer_config.btc_provider.chain)
			.expect(INVALID_BITCOIN_NETWORK);

		let auth = match (
			config.relayer_config.btc_provider.username.clone(),
			config.relayer_config.btc_provider.password.clone(),
		) {
			(Some(username), Some(password)) => Auth::UserPass(username, password),
			_ => Auth::None,
		};
		let btc_client = BitcoinClient::new(
			&config.relayer_config.btc_provider.provider,
			auth,
			config.relayer_config.btc_provider.wallet.clone(),
			Some(60),
		)
		.expect(INVALID_PROVIDER_URL);

		let block_manager = BlockManager::new(
			btc_client.clone(),
			bfc_client.clone(),
			bootstrap_shared_data.clone(),
			config.relayer_config.btc_provider.call_interval,
			config
				.relayer_config
				.btc_provider
				.block_confirmations
				.unwrap_or(DEFAULT_BITCOIN_BLOCK_CONFIRMATIONS),
		);
		let inbound = InboundHandler::new(
			bfc_client.clone(),
			block_manager.subscribe(),
			bootstrap_shared_data.clone(),
			task_manager.spawn_handle(),
			debug_mode,
		);
		let outbound = OutboundHandler::new(
			bfc_client.clone(),
			block_manager.subscribe(),
			bootstrap_shared_data.clone(),
			task_manager.spawn_handle(),
			debug_mode,
		);

		let psbt_signer = PsbtSigner::new(
			bfc_client.clone(),
			substrate_deps.xt_request_sender.clone(),
			keypair_storage.clone(),
			migration_sequence.clone(),
			network,
		);
		let pub_key_submitter = PubKeySubmitter::new(
			bfc_client.clone(),
			substrate_deps.xt_request_sender.clone(),
			keypair_storage.clone(),
			migration_sequence.clone(),
		);
		let rollback_verifier = BitcoinRollbackVerifier::new(
			btc_client.clone(),
			bfc_client.clone(),
			substrate_deps.xt_request_sender.clone(),
		);

		Self { outbound, inbound, block_manager, psbt_signer, pub_key_submitter, rollback_verifier }
	}
}
