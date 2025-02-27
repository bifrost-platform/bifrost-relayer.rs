use std::{
	collections::BTreeMap,
	net::{Ipv4Addr, SocketAddr},
	sync::Arc,
	time::Duration,
};

use bitcoincore_rpc::{Auth, Client as BitcoinClient};
use ethers::providers::{Http, Provider};
use futures::FutureExt;
use miniscript::bitcoin::Network;
use sc_service::{config::PrometheusConfig, Error as ServiceError, TaskManager};
use tokio::sync::RwLock;

use br_client::{
	btc::{
		block::BlockManager,
		handlers::{Handler as BitcoinHandler, InboundHandler, OutboundHandler},
		storage::keypair::KeypairStorage,
		storage::pending_outbound::PendingOutboundPool,
	},
	eth::{
		events::EventManager,
		handlers::{RoundupRelayHandler, SocketRelayHandler},
		traits::{Handler, TransactionManager},
		tx::{Eip1559TransactionManager, LegacyTransactionManager},
		wallet::WalletManager,
		EthClient,
	},
	substrate::tx::UnsignedTransactionManager,
};
use br_periodic::{
	traits::PeriodicWorker, BitcoinRollbackVerifier, HeartbeatSender, KeypairMigrator,
	OraclePriceFeeder, PsbtSigner, PubKeyPreSubmitter, PubKeySubmitter, RoundupEmitter,
	SocketRollbackEmitter,
};
use br_primitives::{
	bootstrap::BootstrapSharedData,
	cli::{Configuration, HandlerType},
	constants::{
		cli::{
			DEFAULT_BITCOIN_BLOCK_CONFIRMATIONS, DEFAULT_GET_LOGS_BATCH_SIZE,
			DEFAULT_KEYSTORE_PATH, DEFAULT_MIN_PRIORITY_FEE, DEFAULT_PROMETHEUS_PORT,
		},
		errors::{
			INVALID_BIFROST_NATIVENESS, INVALID_BITCOIN_NETWORK, INVALID_CHAIN_ID,
			INVALID_PRIVATE_KEY, INVALID_PROVIDER_URL,
		},
	},
	eth::{AggregatorContracts, BootstrapState, ChainID, ProtocolContracts, ProviderMetadata},
	periodic::RollbackSender,
	substrate::MigrationSequence,
	tx::{TxRequestSender, XtRequestSender},
	utils::sub_display_format,
};

use crate::{
	cli::{LOG_TARGET, SUB_LOG_TARGET},
	verification::assert_configuration_validity,
};

/// Starts the relayer service.
pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

/// Initializes periodic components.
fn construct_periodics(
	bootstrap_shared_data: BootstrapSharedData,
	migration_sequence: Arc<RwLock<MigrationSequence>>,
	keypair_storage: Arc<RwLock<KeypairStorage>>,
	relayer_deps: &ManagerDeps,
	substrate_deps: &SubstrateDeps,
) -> PeriodicDeps {
	let clients = &relayer_deps.clients;
	let tx_request_senders = &relayer_deps.tx_request_senders;

	let mut rollback_emitters = vec![];
	let mut rollback_senders = BTreeMap::new();

	// initialize the heartbeat sender
	let heartbeat_sender = HeartbeatSender::new(tx_request_senders.clone(), clients.clone());

	// initialize the oracle price feeder
	let oracle_price_feeder = OraclePriceFeeder::new(tx_request_senders.clone(), clients.clone());

	// initialize the roundup emitter
	let roundup_emitter = RoundupEmitter::new(
		tx_request_senders.clone(),
		clients.clone(),
		Arc::new(bootstrap_shared_data.clone()),
	);

	// initialize socket rollback handlers
	tx_request_senders.iter().for_each(|tx_request_sender| {
		let (rollback_emitter, rollback_sender) =
			SocketRollbackEmitter::new(tx_request_sender.clone(), clients.clone());
		rollback_emitters.push(rollback_emitter);
		rollback_senders.insert(
			tx_request_sender.id,
			Arc::new(RollbackSender::new(tx_request_sender.id, rollback_sender)),
		);
	});

	// initialize migration detector
	let bfc_client = clients
		.iter()
		.find(|client| client.metadata.is_native)
		.expect(INVALID_BIFROST_NATIVENESS)
		.clone();
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

	PeriodicDeps {
		heartbeat_sender,
		oracle_price_feeder,
		roundup_emitter,
		rollback_emitters,
		rollback_senders,
		keypair_migrator,
		presubmitter,
	}
}

/// Initializes `Socket` & `RoundUp` handlers.
fn construct_handlers(
	config: &Configuration,
	periodic_deps: &PeriodicDeps,
	manager_deps: &ManagerDeps,
	bootstrap_shared_data: BootstrapSharedData,
) -> HandlerDeps {
	let mut handlers = (vec![], vec![]);
	let PeriodicDeps { rollback_senders, .. } = periodic_deps;
	let ManagerDeps { clients, event_managers, tx_request_senders, .. } = manager_deps;

	config.relayer_config.handler_configs.iter().for_each(|handler_config| {
		match handler_config.handler_type {
			HandlerType::Socket => handler_config.watch_list.iter().for_each(|target| {
				handlers.0.push(SocketRelayHandler::new(
					*target,
					tx_request_senders.clone(),
					rollback_senders.clone(),
					event_managers.get(target).expect(INVALID_CHAIN_ID).sender.subscribe(),
					clients.clone(),
					Arc::new(bootstrap_shared_data.clone()),
				));
			}),
			HandlerType::Roundup => {
				handlers.1.push(RoundupRelayHandler::new(
					tx_request_senders.clone(),
					event_managers
						.get(&handler_config.watch_list[0])
						.expect(INVALID_CHAIN_ID)
						.sender
						.subscribe(),
					clients.clone(),
					Arc::new(bootstrap_shared_data.clone()),
				));
			},
		}
	});
	HandlerDeps { socket_relay_handlers: handlers.0, roundup_relay_handlers: handlers.1 }
}

/// Initializes the `EthClient`, `TransactionManager`, `EventManager`, `TxRequestSender` for each chain.
fn construct_managers(
	config: &Configuration,
	bootstrap_shared_data: BootstrapSharedData,
	task_manager: &TaskManager,
) -> ManagerDeps {
	let prometheus_config = &config.relayer_config.prometheus_config;
	let evm_providers = &config.relayer_config.evm_providers;
	let btc_provider = &config.relayer_config.btc_provider;
	let system = &config.relayer_config.system;

	let mut clients = vec![];
	let mut tx_managers = (vec![], vec![]);
	let mut event_managers = BTreeMap::new();
	let mut tx_request_senders = vec![];

	// iterate each evm provider and construct inner components.
	evm_providers.iter().for_each(|evm_provider| {
		let is_native = evm_provider.is_native.unwrap_or(false);
		let provider = Provider::<Http>::try_from(evm_provider.provider.clone())
			.expect(INVALID_PROVIDER_URL)
			.interval(Duration::from_millis(evm_provider.call_interval));

		let client = Arc::new(EthClient::new(
			WalletManager::from_private_key(system.private_key.as_str(), evm_provider.id)
				.expect(INVALID_PRIVATE_KEY),
			Arc::new(provider.clone()),
			ProviderMetadata::new(
				evm_provider.name.clone(),
				provider.url().to_string(),
				evm_provider.id,
				if is_native { Some(btc_provider.id) } else { None },
				evm_provider.block_confirmations,
				evm_provider.call_interval,
				evm_provider.get_logs_batch_size.unwrap_or(DEFAULT_GET_LOGS_BATCH_SIZE),
				is_native,
			),
			ProtocolContracts::new(
				is_native,
				Arc::new(provider.clone()),
				evm_provider.socket_address.clone(),
				evm_provider.authority_address.clone(),
				evm_provider.relayer_manager_address.clone(),
				evm_provider.bitcoin_socket_address.clone(),
				evm_provider.socket_queue_address.clone(),
				evm_provider.registration_pool_address.clone(),
				evm_provider.relay_executive_address.clone(),
			),
			AggregatorContracts::new(
				Arc::new(provider),
				evm_provider.chainlink_usdc_usd_address.clone(),
				evm_provider.chainlink_usdt_usd_address.clone(),
				evm_provider.chainlink_dai_usd_address.clone(),
				evm_provider.chainlink_btc_usd_address.clone(),
				evm_provider.chainlink_wbtc_usd_address.clone(),
				evm_provider.chainlink_cbbtc_usd_address.clone(),
			),
			system.debug_mode.unwrap_or(false),
		));

		if evm_provider.is_relay_target {
			if evm_provider.eip1559.unwrap_or(false) {
				let (tx_manager, sender) = Eip1559TransactionManager::new(
					client.clone(),
					evm_provider.min_priority_fee.unwrap_or(DEFAULT_MIN_PRIORITY_FEE).into(),
					evm_provider.duplicate_confirm_delay,
					task_manager.spawn_handle(),
				);
				tx_managers.1.push(tx_manager);
				tx_request_senders.push(Arc::new(TxRequestSender::new(
					evm_provider.id,
					sender,
					is_native,
				)));
			} else {
				let (tx_manager, sender) = LegacyTransactionManager::new(
					client.clone(),
					evm_provider.escalate_percentage,
					evm_provider.min_gas_price,
					evm_provider.is_initially_escalated.unwrap_or(false),
					evm_provider.duplicate_confirm_delay,
					task_manager.spawn_handle(),
				);
				tx_managers.0.push(tx_manager);
				tx_request_senders.push(Arc::new(TxRequestSender::new(
					evm_provider.id,
					sender,
					is_native,
				)));
			}
		}
		let event_manager = EventManager::new(
			client.clone(),
			Arc::new(bootstrap_shared_data.clone()),
			match &prometheus_config {
				Some(config) => config.is_enabled,
				None => false,
			},
		);

		clients.push(client);
		event_managers.insert(event_manager.client.get_chain_id(), event_manager);
	});

	ManagerDeps { clients, tx_managers, event_managers, tx_request_senders }
}

/// Initializes Bitcoin related instances.
fn construct_btc_deps(
	config: &Configuration,
	pending_outbounds: PendingOutboundPool,
	keypair_storage: Arc<RwLock<KeypairStorage>>,
	bootstrap_shared_data: BootstrapSharedData,
	manager_deps: &ManagerDeps,
	substrate_deps: &SubstrateDeps,
	migration_sequence: Arc<RwLock<MigrationSequence>>,
) -> BtcDeps {
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

	let bfc_client = manager_deps
		.clients
		.iter()
		.find(|client| client.metadata.is_native)
		.expect(INVALID_BIFROST_NATIVENESS)
		.clone();
	let tx_request_sender = manager_deps
		.tx_request_senders
		.iter()
		.find(|sender| sender.is_native)
		.expect(INVALID_BIFROST_NATIVENESS)
		.clone();

	let block_manager = BlockManager::new(
		btc_client.clone(),
		bfc_client.clone(),
		pending_outbounds.clone(),
		bootstrap_shared_data.clone(),
		config.relayer_config.btc_provider.call_interval.clone(),
		config
			.relayer_config
			.btc_provider
			.block_confirmations
			.unwrap_or(DEFAULT_BITCOIN_BLOCK_CONFIRMATIONS),
	);
	let inbound = InboundHandler::new(
		bfc_client.clone(),
		tx_request_sender.clone(),
		block_manager.subscribe(),
		bootstrap_shared_data.clone(),
	);
	let outbound = OutboundHandler::new(
		bfc_client.clone(),
		tx_request_sender.clone(),
		block_manager.subscribe(),
		bootstrap_shared_data.clone(),
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

	BtcDeps { outbound, inbound, block_manager, psbt_signer, pub_key_submitter, rollback_verifier }
}

/// Initializes Substrate related instances.
fn construct_substrate_deps(
	manager_deps: &ManagerDeps,
	task_manager: &TaskManager,
) -> SubstrateDeps {
	let bfc_client = manager_deps
		.clients
		.iter()
		.find(|client| client.metadata.is_native)
		.expect(INVALID_BIFROST_NATIVENESS)
		.clone();

	let (unsigned_tx_manager, sender) =
		UnsignedTransactionManager::new(bfc_client.clone(), task_manager.spawn_handle());

	let xt_request_sender = Arc::new(XtRequestSender::new(sender));

	SubstrateDeps { unsigned_tx_manager, xt_request_sender }
}

/// Spawn relayer service tasks by the `TaskManager`.
fn spawn_relayer_tasks(
	task_manager: TaskManager,
	deps: FullDeps,
	config: &Configuration,
) -> TaskManager {
	let prometheus_config = &config.relayer_config.prometheus_config;

	let FullDeps {
		bootstrap_shared_data,
		manager_deps,
		periodic_deps,
		handler_deps,
		substrate_deps,
		btc_deps,
	} = deps;

	let BootstrapSharedData { socket_barrier, bootstrap_states, .. } = bootstrap_shared_data;
	let ManagerDeps { tx_managers, event_managers, .. } = manager_deps;
	let PeriodicDeps {
		mut heartbeat_sender,
		mut oracle_price_feeder,
		mut roundup_emitter,
		rollback_emitters,
		mut keypair_migrator,
		mut presubmitter,
		..
	} = periodic_deps;
	let HandlerDeps { socket_relay_handlers, roundup_relay_handlers } = handler_deps;
	let SubstrateDeps { mut unsigned_tx_manager, .. } = substrate_deps;
	let BtcDeps {
		mut outbound,
		mut inbound,
		mut block_manager,
		mut psbt_signer,
		mut pub_key_submitter,
		mut rollback_verifier,
	} = btc_deps;

	// spawn migration detector
	task_manager.spawn_essential_handle().spawn(
		"migration-detector",
		Some("migration-detector"),
		async move { keypair_migrator.run().await },
	);

	// spawn public key presubmitter
	task_manager.spawn_essential_handle().spawn(
		"pub-key-presubmitter",
		Some("pub-key-presubmitter"),
		async move { presubmitter.run().await },
	);

	// spawn legacy transaction managers
	tx_managers.0.into_iter().for_each(|mut tx_manager| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-transaction-manager", tx_manager.client.get_chain_name())
					.into_boxed_str(),
			),
			Some("transaction-managers"),
			async move { tx_manager.run().await },
		)
	});
	// spawn eip1559 transaction managers
	tx_managers.1.into_iter().for_each(|mut tx_manager| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-transaction-manager", tx_manager.client.get_chain_name())
					.into_boxed_str(),
			),
			Some("transaction-managers"),
			async move { tx_manager.run().await },
		)
	});
	// spawn unsigned transaction manager
	task_manager.spawn_essential_handle().spawn(
		"unsigned-transaction-manager",
		Some("transaction-managers"),
		async move { unsigned_tx_manager.run().await },
	);

	// spawn heartbeat sender
	task_manager
		.spawn_essential_handle()
		.spawn("heartbeat", Some("heartbeat"), async move { heartbeat_sender.run().await });

	// spawn oracle price feeder
	task_manager.spawn_essential_handle().spawn(
		Box::leak(
			format!("{}-oracle-price-feeder", oracle_price_feeder.client.get_chain_name())
				.into_boxed_str(),
		),
		Some("oracle"),
		async move { oracle_price_feeder.run().await },
	);
	// spawn socket rollback emitters
	rollback_emitters.into_iter().for_each(|mut emitter| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-socket-rollback-emitter", emitter.client.get_chain_name())
					.into_boxed_str(),
			),
			Some("rollback"),
			async move { emitter.run().await },
		)
	});

	// spawn socket relay handlers
	socket_relay_handlers.into_iter().for_each(|mut handler| {
		let socket_barrier_clone = socket_barrier.clone();
		let is_bootstrapped = bootstrap_states.clone();

		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-{}-handler", handler.client.get_chain_name(), HandlerType::Socket,)
					.into_boxed_str(),
			),
			Some("handlers"),
			async move {
				socket_barrier_clone.wait().await;

				// After All of barrier complete the waiting
				let mut guard = is_bootstrapped.write().await;
				if guard.iter().all(|s| *s == BootstrapState::BootstrapRoundUpPhase2) {
					for state in guard.iter_mut() {
						*state = BootstrapState::BootstrapSocketRelay;
					}
				}
				drop(guard);

				handler.run().await
			},
		);
	});

	// spawn roundup relay handlers
	roundup_relay_handlers.into_iter().for_each(|mut handler| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-{}-handler", handler.client.get_chain_name(), HandlerType::Roundup)
					.into_boxed_str(),
			),
			Some("handlers"),
			async move { handler.run().await },
		);
	});

	// spawn roundup emitter
	task_manager.spawn_essential_handle().spawn(
		"roundup-emitter",
		Some("roundup-emitter"),
		async move { roundup_emitter.run().await },
	);

	// spawn event managers
	event_managers.into_iter().for_each(|(_chain_id, mut event_manager)| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-event-manager", event_manager.client.get_chain_name()).into_boxed_str(),
			),
			Some("event-managers"),
			async move {
				event_manager.wait_provider_sync().await;

				event_manager.run().await
			},
		)
	});

	// spawn bitcoin deps
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-inbound-handler",
		Some("handlers"),
		async move { inbound.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-outbound-handler",
		Some("handlers"),
		async move { outbound.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-psbt-signer",
		Some("handlers"),
		async move { psbt_signer.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-public-key-submitter",
		Some("pub-key-submitter"),
		async move { pub_key_submitter.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-rollback-verifier",
		Some("rollback-verifier"),
		async move { rollback_verifier.run().await },
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-block-manager",
		Some("block-manager"),
		async move {
			socket_barrier.wait().await;

			// After All of barrier complete the waiting
			let mut guard = bootstrap_states.write().await;
			if guard.iter().all(|s| *s == BootstrapState::BootstrapRoundUpPhase2) {
				for state in guard.iter_mut() {
					*state = BootstrapState::BootstrapSocketRelay;
				}
			}
			drop(guard);

			block_manager.run().await
		},
	);

	// spawn prometheus endpoint
	if let Some(prometheus_config) = prometheus_config {
		if prometheus_config.is_enabled {
			let interface = match prometheus_config.is_external.unwrap_or(false) {
				true => Ipv4Addr::UNSPECIFIED,
				false => Ipv4Addr::LOCALHOST,
			};

			let prometheus = PrometheusConfig::new_with_default_registry(
				SocketAddr::new(
					interface.into(),
					prometheus_config.port.unwrap_or(DEFAULT_PROMETHEUS_PORT),
				),
				String::default(),
			);

			br_metrics::setup(&prometheus.registry);

			// spawn prometheus
			task_manager.spawn_handle().spawn(
				"prometheus-endpoint",
				None,
				prometheus_endpoint::init_prometheus(prometheus.port, prometheus.registry)
					.map(drop),
			);
		}
	}
	task_manager
}

/// Log the configured relay targets.
fn print_relay_targets(manager_deps: &ManagerDeps) {
	let tx_managers = &manager_deps.tx_managers;

	log::info!(
		target: LOG_TARGET,
		"-[{}] 👤 Relayer: {:?}",
		sub_display_format(SUB_LOG_TARGET),
		&manager_deps.clients[0].address()
	);

	if !tx_managers.0.is_empty() {
		log::info!(
			target: LOG_TARGET,
			"-[{}] 🔨 Relay Targets (Legacy): {}",
			sub_display_format(SUB_LOG_TARGET),
			tx_managers.0
				.iter()
				.map(|tx_manager| tx_manager.client.get_chain_name())
				.collect::<Vec<String>>()
				.join(", ")
		);
	}
	if !tx_managers.1.is_empty() {
		log::info!(
			target: LOG_TARGET,
			"-[{}] 🔨 Relay Targets (EIP1559): {}",
			sub_display_format(SUB_LOG_TARGET),
			tx_managers.1
				.iter()
				.map(|tx_manager| tx_manager.client.get_chain_name())
				.collect::<Vec<String>>()
				.join(", ")
		);
	}
}

/// Builds the internal components for the relayer service and spawns asynchronous tasks.
fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	assert_configuration_validity(&config);

	let task_manager = TaskManager::new(config.clone().tokio_handle, None)?;

	let bootstrap_shared_data = BootstrapSharedData::new(&config);

	let pending_outbounds = PendingOutboundPool::new();
	let keypair_storage = Arc::new(RwLock::new(KeypairStorage::new(
		config
			.clone()
			.relayer_config
			.system
			.keystore_path
			.unwrap_or(DEFAULT_KEYSTORE_PATH.to_string()),
		config.relayer_config.system.keystore_password.clone(),
		Network::from_core_arg(&config.relayer_config.btc_provider.chain)
			.expect(INVALID_BITCOIN_NETWORK),
	)));

	let migration_sequence = Arc::new(RwLock::new(MigrationSequence::Normal));

	let manager_deps = construct_managers(&config, bootstrap_shared_data.clone(), &task_manager);
	let substrate_deps = construct_substrate_deps(&manager_deps, &task_manager);
	let periodic_deps = construct_periodics(
		bootstrap_shared_data.clone(),
		migration_sequence.clone(),
		keypair_storage.clone(),
		&manager_deps,
		&substrate_deps,
	);
	let handler_deps =
		construct_handlers(&config, &periodic_deps, &manager_deps, bootstrap_shared_data.clone());
	let btc_deps = construct_btc_deps(
		&config,
		pending_outbounds.clone(),
		keypair_storage.clone(),
		bootstrap_shared_data.clone(),
		&manager_deps,
		&substrate_deps,
		migration_sequence.clone(),
	);

	print_relay_targets(&manager_deps);

	Ok(RelayBase {
		task_manager: spawn_relayer_tasks(
			task_manager,
			FullDeps {
				bootstrap_shared_data,
				manager_deps,
				periodic_deps,
				handler_deps,
				substrate_deps,
				btc_deps,
			},
			&config,
		),
	})
}

struct RelayBase {
	/// The task manager of the relayer.
	task_manager: TaskManager,
}

struct ManagerDeps {
	/// The `EthClient`'s for each specified chain.
	clients: Vec<Arc<EthClient<Http>>>,
	/// The `TransactionManager`'s for each specified chain.
	tx_managers: (Vec<LegacyTransactionManager<Http>>, Vec<Eip1559TransactionManager<Http>>),
	/// The `EventManager`'s for each specified chain.
	event_managers: BTreeMap<ChainID, EventManager<Http>>,
	/// The `TxRequestSender`'s for each specified chain.
	tx_request_senders: Vec<Arc<TxRequestSender>>,
}

struct BtcDeps {
	/// The Bitcoin outbound handler.
	outbound: OutboundHandler<Http>,
	/// The Bitcoin inbound handler.
	inbound: InboundHandler<Http>,
	/// The Bitcoin block manager.
	block_manager: BlockManager<Http>,
	/// The Bitcoin PSBT signer.
	psbt_signer: PsbtSigner<Http>,
	/// The Bitcoin vault public key submitter.
	pub_key_submitter: PubKeySubmitter<Http>,
	/// The Bitcoin rollback verifier.
	rollback_verifier: BitcoinRollbackVerifier<Http>,
}

struct SubstrateDeps {
	/// The `UnsignedTransactionManager` for Bifrost.
	unsigned_tx_manager: UnsignedTransactionManager<Http>,
	/// The `XtRequestSender` for Bifrost.
	xt_request_sender: Arc<XtRequestSender>,
}

struct PeriodicDeps {
	/// The `HeartbeatSender` used for system health checks.
	heartbeat_sender: HeartbeatSender<Http>,
	/// The `OraclePriceFeeder` used for price feeding.
	oracle_price_feeder: OraclePriceFeeder<Http>,
	/// The `RoundupEmitter` used for detecting and emitting new round updates.
	roundup_emitter: RoundupEmitter<Http>,
	/// The `SocketRollbackEmitter`'s for each specified chain.
	rollback_emitters: Vec<SocketRollbackEmitter<Http>>,
	/// The `RollbackSender`'s for each specified chain.
	rollback_senders: BTreeMap<ChainID, Arc<RollbackSender>>,
	/// The `KeypairMigrator` used for detecting migration sequences.
	keypair_migrator: KeypairMigrator<Http>,
	/// The `PubKeyPreSubmitter` used for presubmitting public keys.
	presubmitter: PubKeyPreSubmitter<Http>,
}

struct HandlerDeps {
	/// The `SocketRelayHandler`'s for each specified chain.
	socket_relay_handlers: Vec<SocketRelayHandler<Http>>,
	/// The `RoundupRelayHandler`'s for each specified chain.
	roundup_relay_handlers: Vec<RoundupRelayHandler<Http>>,
}

/// The relayer client dependencies.
struct FullDeps {
	bootstrap_shared_data: BootstrapSharedData,
	manager_deps: ManagerDeps,
	periodic_deps: PeriodicDeps,
	handler_deps: HandlerDeps,
	substrate_deps: SubstrateDeps,
	btc_deps: BtcDeps,
}
