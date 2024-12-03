use std::{
	collections::BTreeMap,
	net::{Ipv4Addr, SocketAddr},
	str::FromStr,
	sync::Arc,
	time::Duration,
};

use alloy::{
	network::EthereumWallet,
	primitives::ChainId,
	providers::{fillers::TxFiller, Provider, ProviderBuilder, WalletProvider},
	rpc::client::RpcClient,
	signers::{local::PrivateKeySigner, Signer},
	transports::{http::reqwest::Url, Transport},
};
use futures::FutureExt;
use miniscript::bitcoin::Network;
use sc_service::{config::PrometheusConfig, Error as ServiceError, TaskManager};
use tokio::sync::RwLock;

use br_client::{
	btc::{
		handlers::Handler as _,
		storage::{keypair::KeypairStorage, pending_outbound::PendingOutboundPool},
	},
	eth::{traits::Handler as _, EthClient},
};
use br_periodic::traits::PeriodicWorker;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	cli::{Configuration, HandlerType},
	constants::{
		cli::{DEFAULT_GET_LOGS_BATCH_SIZE, DEFAULT_KEYSTORE_PATH, DEFAULT_PROMETHEUS_PORT},
		errors::{
			INVALID_BIFROST_NATIVENESS, INVALID_BITCOIN_NETWORK, INVALID_PRIVATE_KEY,
			INVALID_PROVIDER_URL,
		},
		tx::DEFAULT_CALL_RETRIES,
	},
	eth::{
		retry::RetryBackoffLayer, AggregatorContracts, BootstrapState, ProtocolContracts,
		ProviderMetadata,
	},
	substrate::MigrationSequence,
	utils::sub_display_format,
};

use crate::{
	cli::{LOG_TARGET, SUB_LOG_TARGET},
	service_deps::{BtcDeps, FullDeps, HandlerDeps, ManagerDeps, PeriodicDeps, SubstrateDeps},
	verification::assert_configuration_validity,
};

/// Starts the relayer service.
pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	assert_configuration_validity(&config);

	let evm_providers = &config.relayer_config.evm_providers;
	let btc_provider = &config.relayer_config.btc_provider;
	let system = &config.relayer_config.system;

	let clients = evm_providers
		.iter()
		.map(|evm_provider| {
			let url: Url = evm_provider.provider.clone().parse().expect(INVALID_PROVIDER_URL);
			let is_native = evm_provider.is_native.unwrap_or(false);

			let mut signer =
				PrivateKeySigner::from_str(&system.private_key).expect(INVALID_PRIVATE_KEY);
			signer.set_chain_id(Some(evm_provider.id));
			let wallet = EthereumWallet::from(signer.clone());

			let client = RpcClient::builder()
				.layer(RetryBackoffLayer::new(DEFAULT_CALL_RETRIES, evm_provider.call_interval))
				.http(url.clone())
				.with_poll_interval(Duration::from_millis(evm_provider.call_interval));
			let provider = Arc::new(
				ProviderBuilder::new()
					.with_recommended_fillers()
					.wallet(wallet)
					.on_client(client),
			);
			let client = Arc::new(EthClient::new(
				provider.clone(),
				signer.clone(),
				ProviderMetadata::new(
					evm_provider.name.clone(),
					url.clone(),
					evm_provider.id,
					if is_native { Some(btc_provider.id) } else { None },
					evm_provider.block_confirmations,
					evm_provider.call_interval,
					evm_provider.get_logs_batch_size.unwrap_or(DEFAULT_GET_LOGS_BATCH_SIZE),
					is_native,
				),
				ProtocolContracts::new(
					is_native,
					provider.clone(),
					evm_provider.socket_address.clone(),
					evm_provider.authority_address.clone(),
					evm_provider.relayer_manager_address.clone(),
					evm_provider.bitcoin_socket_address.clone(),
					evm_provider.socket_queue_address.clone(),
					evm_provider.registration_pool_address.clone(),
					evm_provider.relay_executive_address.clone(),
				),
				AggregatorContracts::new(
					provider.clone(),
					evm_provider.chainlink_usdc_usd_address.clone(),
					evm_provider.chainlink_usdt_usd_address.clone(),
					evm_provider.chainlink_dai_usd_address.clone(),
					evm_provider.chainlink_btc_usd_address.clone(),
					evm_provider.chainlink_wbtc_usd_address.clone(),
					evm_provider.chainlink_cbbtc_usd_address.clone(),
				),
			));
			(evm_provider.id, client)
		})
		.collect::<BTreeMap<ChainId, _>>();

	new_relay_base(config, clients).map(|RelayBase { task_manager, .. }| task_manager)
}

/// Spawn relayer service tasks by the `TaskManager`.
fn spawn_relayer_tasks<F, P, T>(
	task_manager: TaskManager,
	deps: FullDeps<F, P, T>,
	config: &Configuration,
) -> TaskManager
where
	F: TxFiller + WalletProvider + 'static,
	P: Provider<T> + 'static,
	T: Transport + Clone,
{
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
	let ManagerDeps { event_managers, .. } = manager_deps;
	let PeriodicDeps {
		mut heartbeat_sender,
		mut oracle_price_feeder,
		mut roundup_emitter,
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
		async move {
			keypair_migrator.run().await;
			()
		},
	);

	// spawn public key presubmitter
	task_manager.spawn_essential_handle().spawn(
		"pub-key-presubmitter",
		Some("pub-key-presubmitter"),
		async move {
			presubmitter.run().await;
			()
		},
	);

	// spawn unsigned transaction manager
	task_manager.spawn_essential_handle().spawn(
		"unsigned-transaction-manager",
		Some("transaction-managers"),
		async move { unsigned_tx_manager.run().await },
	);

	// spawn heartbeat sender
	task_manager
		.spawn_essential_handle()
		.spawn("heartbeat", Some("heartbeat"), async move {
			heartbeat_sender.run().await;
			()
		});

	// spawn oracle price feeder
	task_manager.spawn_essential_handle().spawn(
		Box::leak(
			format!("{}-oracle-price-feeder", oracle_price_feeder.client.get_chain_name())
				.into_boxed_str(),
		),
		Some("oracle"),
		async move {
			oracle_price_feeder.run().await;
			()
		},
	);

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

				handler.run().await;

				()
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
			async move {
				handler.run().await;
				()
			},
		);
	});

	// spawn roundup emitter
	task_manager.spawn_essential_handle().spawn(
		"roundup-emitter",
		Some("roundup-emitter"),
		async move {
			roundup_emitter.run().await;
			()
		},
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
				event_manager.run().await;
				()
			},
		)
	});

	// spawn bitcoin deps
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-inbound-handler",
		Some("handlers"),
		async move {
			inbound.run().await;
			()
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-outbound-handler",
		Some("handlers"),
		async move {
			outbound.run().await;
			()
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-psbt-signer",
		Some("handlers"),
		async move {
			psbt_signer.run().await;
			()
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-public-key-submitter",
		Some("pub-key-submitter"),
		async move {
			pub_key_submitter.run().await;
			()
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-rollback-verifier",
		Some("rollback-verifier"),
		async move {
			rollback_verifier.run().await;
			()
		},
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

			block_manager.run().await;

			()
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
fn print_relay_targets<F, P, T>(manager_deps: &ManagerDeps<F, P, T>)
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	log::info!(
		target: LOG_TARGET,
		"-[{}] 👤 Relayer: {:?}",
		sub_display_format(SUB_LOG_TARGET),
		manager_deps.bifrost_client.address()
	);
	log::info!(
		target: LOG_TARGET,
		"-[{}] 🔨 Relay Targets: {}",
		sub_display_format(SUB_LOG_TARGET),
		manager_deps
			.clients
			.iter()
			.map(|(chain_id, client)| format!("{} ({})", client.get_chain_name(), chain_id))
			.collect::<Vec<String>>()
			.join(", ")
	);
}

/// Builds the internal components for the relayer service and spawns asynchronous tasks.
fn new_relay_base<F, P, T>(
	config: Configuration,
	clients: BTreeMap<ChainId, Arc<EthClient<F, P, T>>>,
) -> Result<RelayBase, ServiceError>
where
	F: TxFiller + WalletProvider + 'static,
	P: Provider<T> + 'static,
	T: Transport + Clone,
{
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

	let manager_deps = ManagerDeps::new(&config, Arc::new(clients), bootstrap_shared_data.clone());
	let bfc_client = manager_deps.bifrost_client.clone();

	let substrate_deps = SubstrateDeps::new(bfc_client.clone(), &task_manager);
	let periodic_deps = PeriodicDeps::new(
		bootstrap_shared_data.clone(),
		migration_sequence.clone(),
		keypair_storage.clone(),
		&substrate_deps,
		manager_deps.clients.clone(),
		bfc_client.clone(),
	);
	let handler_deps =
		HandlerDeps::new(&config, &manager_deps, bootstrap_shared_data.clone(), bfc_client.clone());
	let btc_deps = BtcDeps::new(
		&config,
		pending_outbounds.clone(),
		keypair_storage.clone(),
		bootstrap_shared_data.clone(),
		&manager_deps,
		&substrate_deps,
		migration_sequence.clone(),
		bfc_client.clone(),
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
