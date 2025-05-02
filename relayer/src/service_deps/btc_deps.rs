use br_client::btc::handlers::FeeRateFeeder;
use br_primitives::btc::{
	MEMPOOL_SPACE_BLOCK_HEIGHT_ENDPOINT, MEMPOOL_SPACE_FEE_RATE_ENDPOINT,
	MEMPOOL_SPACE_TESTNET_BLOCK_HEIGHT_ENDPOINT, MEMPOOL_SPACE_TESTNET_FEE_RATE_ENDPOINT,
};

use super::*;

pub struct BtcDeps<F, P, N: AlloyNetwork>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	/// The Bitcoin outbound handler.
	pub outbound: OutboundHandler<F, P, N>,
	/// The Bitcoin inbound handler.
	pub inbound: InboundHandler<F, P, N>,
	/// The Bitcoin block manager.
	pub block_manager: BlockManager<F, P, N>,
	/// The Bitcoin PSBT signer.
	pub psbt_signer: PsbtSigner<F, P, N>,
	/// The Bitcoin vault public key submitter.
	pub pub_key_submitter: PubKeySubmitter<F, P, N>,
	/// The Bitcoin rollback verifier.
	pub rollback_verifier: BitcoinRollbackVerifier<F, P, N>,
	/// The Bitcoin fee rate feeder.
	pub fee_rate_feeder: FeeRateFeeder<F, P, N>,
}

impl<F, P, N: AlloyNetwork> BtcDeps<F, P, N>
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	pub fn new(
		config: &Configuration,
		keypair_storage: KeypairStorage,
		bootstrap_shared_data: BootstrapSharedData,
		substrate_deps: &SubstrateDeps<F, P, N>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		bfc_client: Arc<EthClient<F, P, N>>,
		task_manager: &TaskManager,
		debug_mode: bool,
	) -> Self {
		let btc_provider = &config.relayer_config.btc_provider;
		let bootstrap_shared_data = Arc::new(bootstrap_shared_data.clone());
		let network = Network::from_core_arg(&btc_provider.chain).expect(INVALID_BITCOIN_NETWORK);

		let auth = match (btc_provider.username.clone(), btc_provider.password.clone()) {
			(Some(username), Some(password)) => Auth::UserPass(username, password),
			_ => Auth::None,
		};
		let btc_client =
			BitcoinClient::new(&btc_provider.provider, auth, btc_provider.wallet.clone(), Some(60))
				.expect(INVALID_PROVIDER_URL);

		let block_manager = BlockManager::new(
			btc_client.clone(),
			bfc_client.clone(),
			bootstrap_shared_data.clone(),
			btc_provider.call_interval,
			btc_provider.block_confirmations.unwrap_or(DEFAULT_BITCOIN_BLOCK_CONFIRMATIONS),
			if network == Network::Bitcoin {
				MEMPOOL_SPACE_BLOCK_HEIGHT_ENDPOINT
			} else {
				MEMPOOL_SPACE_TESTNET_BLOCK_HEIGHT_ENDPOINT
			},
		);
		let inbound = InboundHandler::new(
			bfc_client.clone(),
			substrate_deps.xt_request_sender.clone(),
			block_manager.subscribe(),
			bootstrap_shared_data.clone(),
			task_manager.spawn_handle(),
			debug_mode,
		);
		let outbound = OutboundHandler::new(
			bfc_client.clone(),
			substrate_deps.xt_request_sender.clone(),
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
		let fee_rate_feeder = FeeRateFeeder::new(
			bfc_client.clone(),
			substrate_deps.xt_request_sender.clone(),
			block_manager.subscribe(),
			if network == Network::Bitcoin {
				MEMPOOL_SPACE_FEE_RATE_ENDPOINT
			} else {
				MEMPOOL_SPACE_TESTNET_FEE_RATE_ENDPOINT
			},
			debug_mode,
		);

		Self {
			outbound,
			inbound,
			block_manager,
			psbt_signer,
			pub_key_submitter,
			rollback_verifier,
			fee_rate_feeder,
		}
	}
}
