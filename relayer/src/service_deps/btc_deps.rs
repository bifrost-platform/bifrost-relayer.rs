use br_client::btc::handlers::FeeRateFeeder;
use br_periodic::PsbtBroadcaster;
use br_primitives::btc::{
	MEMPOOL_SPACE_BLOCK_HEIGHT_ENDPOINT, MEMPOOL_SPACE_FEE_RATE_ENDPOINT,
	MEMPOOL_SPACE_TESTNET_BLOCK_HEIGHT_ENDPOINT, MEMPOOL_SPACE_TESTNET_FEE_RATE_ENDPOINT,
};

use super::*;

pub struct BtcDeps<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	/// The Bitcoin outbound handler.
	pub outbound: OutboundHandler<F, P>,
	/// The Bitcoin inbound handler.
	pub inbound: InboundHandler<F, P>,
	/// The Bitcoin block manager.
	pub block_manager: BlockManager<F, P>,
	/// The Bitcoin PSBT signer.
	pub psbt_signer: PsbtSigner<F, P>,
	/// The Bitcoin PSBT broadcaster.
	pub psbt_broadcaster: PsbtBroadcaster<F, P>,
	/// The Bitcoin vault public key submitter.
	pub pub_key_submitter: PubKeySubmitter<F, P>,
	/// The Bitcoin rollback verifier.
	pub rollback_verifier: BitcoinRollbackVerifier<F, P>,
	/// The Bitcoin fee rate feeder.
	pub fee_rate_feeder: FeeRateFeeder<F, P>,
}

impl<F, P> BtcDeps<F, P>
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<AnyNetwork>,
{
	pub fn new(
		config: &Configuration,
		keypair_storage: KeypairStorage,
		bootstrap_shared_data: BootstrapSharedData,
		substrate_deps: &SubstrateDeps<F, P>,
		migration_sequence: Arc<RwLock<MigrationSequence>>,
		bfc_client: Arc<EthClient<F, P>>,
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
		let psbt_broadcaster = PsbtBroadcaster::new(bfc_client.clone(), btc_client.clone());
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
				Some(MEMPOOL_SPACE_FEE_RATE_ENDPOINT)
			} else if network == Network::Testnet {
				Some(MEMPOOL_SPACE_TESTNET_FEE_RATE_ENDPOINT)
			} else {
				None // regtest
			},
			debug_mode,
		);

		Self {
			outbound,
			inbound,
			block_manager,
			psbt_signer,
			psbt_broadcaster,
			pub_key_submitter,
			rollback_verifier,
			fee_rate_feeder,
		}
	}
}
