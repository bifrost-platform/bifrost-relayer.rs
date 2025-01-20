use std::{
	collections::BTreeMap,
	net::{Ipv4Addr, SocketAddr},
	str::FromStr,
	sync::Arc,
	time::Duration,
};

use alloy::{
	network::{AnyNetwork, EthereumWallet},
	providers::{
		fillers::{ChainIdFiller, GasFiller, TxFiller},
		Provider, ProviderBuilder, WalletProvider,
	},
	rpc::client::RpcClient,
	signers::{aws::AwsSigner, local::PrivateKeySigner, Signer},
	transports::{http::reqwest::Url, Transport},
};
use futures::FutureExt;
use miniscript::bitcoin::Network;
use sc_service::{config::PrometheusConfig, Error as ServiceError, TaskManager};
use tokio::sync::RwLock;

use br_client::{
	btc::{
		handlers::Handler as _,
		storage::keypair::{KeypairManager, KeypairStorage, KeypairStorageKind},
	},
	eth::{retry::RetryBackoffLayer, traits::Handler as _, EthClient},
};
use br_periodic::traits::PeriodicWorker;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	cli::{Configuration, HandlerType},
	constants::{
		cli::{DEFAULT_KEYSTORE_PATH, DEFAULT_PROMETHEUS_PORT},
		errors::{
			INVALID_BITCOIN_NETWORK, INVALID_PRIVATE_KEY, INVALID_PROVIDER_URL,
			KMS_INITIALIZATION_ERROR,
		},
		tx::DEFAULT_CALL_RETRIES,
	},
	eth::{AggregatorContracts, ProtocolContracts, ProviderMetadata},
	substrate::MigrationSequence,
	utils::sub_display_format,
};

use crate::{
	cli::{LOG_TARGET, SUB_LOG_TARGET},
	service_deps::{BtcDeps, FullDeps, HandlerDeps, ManagerDeps, PeriodicDeps, SubstrateDeps},
	verification::assert_configuration_validity,
};

/// Starts the relayer service.
pub async fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	assert_configuration_validity(&config);

	let task_manager = TaskManager::new(config.clone().tokio_handle, None)?;

	let evm_providers = &config.relayer_config.evm_providers;
	let btc_provider = &config.relayer_config.btc_provider;
	let system = &config.relayer_config.system;

	let mut clients = BTreeMap::new();
	for evm_provider in evm_providers {
		let url: Url = evm_provider.provider.clone().parse().expect(INVALID_PROVIDER_URL);
		let is_native = evm_provider.is_native.unwrap_or(false);

		let metadata = ProviderMetadata::new(
			evm_provider.clone(),
			url.clone(),
			if is_native { Some(btc_provider.id) } else { None },
			is_native,
		);

		let retry_client = RpcClient::builder()
			.layer(RetryBackoffLayer::new(
				DEFAULT_CALL_RETRIES,
				evm_provider.call_interval,
				evm_provider.name.clone(),
			))
			.http(url.clone())
			.with_poll_interval(Duration::from_millis(evm_provider.call_interval));
		let provider_builder = ProviderBuilder::new()
			.with_cached_nonce_management()
			.filler(GasFiller)
			.filler(ChainIdFiller::new(evm_provider.id.into()))
			.network::<AnyNetwork>();

		let (id, client) = if let Some(key_id) = &config.relayer_config.signer_config.kms_key_id {
			let aws_client = aws_sdk_kms::Client::new(
				&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
			);
			let signer = Arc::new(
				AwsSigner::new(aws_client, key_id.clone(), evm_provider.id.into())
					.await
					.expect(KMS_INITIALIZATION_ERROR),
			);
			let provider = Arc::new(
				provider_builder
					.wallet(EthereumWallet::from(signer.clone()))
					.on_client(retry_client),
			);
			let client = Arc::new(EthClient::new(
				provider.clone(),
				signer.clone(),
				metadata,
				ProtocolContracts::new(is_native, provider.clone(), evm_provider.clone()),
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
		} else {
			let mut signer = PrivateKeySigner::from_str(
				&config
					.relayer_config
					.signer_config
					.private_key
					.clone()
					.expect(INVALID_PRIVATE_KEY),
			)
			.expect(INVALID_PRIVATE_KEY);
			signer.set_chain_id(Some(evm_provider.id));
			let signer = Arc::new(signer);
			let provider = Arc::new(
				provider_builder
					.wallet(EthereumWallet::from(signer.clone()))
					.on_client(retry_client),
			);
			let client = Arc::new(EthClient::new(
				provider.clone(),
				signer.clone(),
				metadata,
				ProtocolContracts::new(is_native, provider.clone(), evm_provider.clone()),
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
		};
		clients.insert(id, client);
	}

	let bootstrap_shared_data = BootstrapSharedData::new(&config);

	let network = Network::from_core_arg(&config.relayer_config.btc_provider.chain)
		.expect(INVALID_BITCOIN_NETWORK);
	let keypair_storage = Arc::new(RwLock::new(
		if let Some(keystore_config) = &config.relayer_config.keystore_config {
			let keystore_path =
				keystore_config.path.clone().unwrap_or(DEFAULT_KEYSTORE_PATH.to_string());
			let keystore = if let Some(key_id) = &keystore_config.kms_key_id {
				let aws_client = Arc::new(aws_sdk_kms::Client::new(
					&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
				));
				KeypairStorage::new(KeypairStorageKind::new_kms(
					keystore_path.clone(),
					network,
					key_id.clone(),
					aws_client,
				))
			} else {
				KeypairStorage::new(KeypairStorageKind::new_password(
					keystore_path,
					network,
					keystore_config.password.clone(),
				))
			};
			keystore
		} else {
			KeypairStorage::new(KeypairStorageKind::new_password(
				DEFAULT_KEYSTORE_PATH.to_string(),
				network,
				None,
			))
		},
	));

	let migration_sequence = Arc::new(RwLock::new(MigrationSequence::Normal));

	let manager_deps = ManagerDeps::new(&config, Arc::new(clients), bootstrap_shared_data.clone());
	let bfc_client = manager_deps.bifrost_client.clone();

	let debug_mode =
		if let Some(system) = system { system.debug_mode.unwrap_or(false) } else { false };
	let substrate_deps = SubstrateDeps::new(bfc_client.clone(), &task_manager);
	let periodic_deps = PeriodicDeps::new(
		bootstrap_shared_data.clone(),
		migration_sequence.clone(),
		keypair_storage.clone(),
		&substrate_deps,
		manager_deps.clients.clone(),
		bfc_client.clone(),
		&task_manager,
		debug_mode,
	);
	let handler_deps = HandlerDeps::new(
		&config,
		&manager_deps,
		bootstrap_shared_data.clone(),
		bfc_client.clone(),
		periodic_deps.rollback_senders.clone(),
		&task_manager,
		debug_mode,
	);
	let btc_deps = BtcDeps::new(
		&config,
		keypair_storage.clone(),
		bootstrap_shared_data.clone(),
		&substrate_deps,
		migration_sequence.clone(),
		bfc_client.clone(),
		&task_manager,
		debug_mode,
	);

	print_relay_targets(&manager_deps);

	Ok(spawn_relayer_tasks(
		task_manager,
		FullDeps { manager_deps, periodic_deps, handler_deps, substrate_deps, btc_deps },
		&config,
	))
}

/// Spawn relayer service tasks by the `TaskManager`.
fn spawn_relayer_tasks<F, P, T, K>(
	task_manager: TaskManager,
	deps: FullDeps<F, P, T, K>,
	config: &Configuration,
) -> TaskManager
where
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork> + 'static,
	P: Provider<T, AnyNetwork> + 'static,
	T: Transport + Clone,
	K: KeypairManager + 'static,
{
	let prometheus_config = &config.relayer_config.prometheus_config;

	let FullDeps { manager_deps, periodic_deps, handler_deps, substrate_deps, btc_deps } = deps;

	let ManagerDeps { event_managers, .. } = manager_deps;
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
		async move {
			let _ = keypair_migrator.run().await;
		},
	);

	// spawn public key presubmitter
	task_manager.spawn_essential_handle().spawn(
		"pub-key-presubmitter",
		Some("pub-key-presubmitter"),
		async move {
			loop {
				let report = presubmitter.run().await;
				let log_msg = format!(
					"public key presubmitter({}) stopped: {:?}\nRestarting in 12 seconds...",
					presubmitter.bfc_client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);

				tokio::time::sleep(Duration::from_secs(12)).await;
			}
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
			loop {
				let report = heartbeat_sender.run().await;
				let log_msg = format!(
					"heartbeat sender({}:{}) stopped: {:?}\nRestarting in 12 seconds...",
					heartbeat_sender.client.get_chain_name(),
					heartbeat_sender.client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);

				tokio::time::sleep(Duration::from_secs(12)).await;
			}
		});

	// spawn oracle price feeder
	task_manager.spawn_essential_handle().spawn(
		Box::leak(
			format!("{}-oracle-price-feeder", oracle_price_feeder.client.get_chain_name())
				.into_boxed_str(),
		),
		Some("oracle"),
		async move {
			loop {
				let report = oracle_price_feeder.run().await;
				let log_msg = format!(
					"oracle price feeder({}:{}) stopped: {:?}\nRestarting in 12 seconds...",
					oracle_price_feeder.client.get_chain_name(),
					oracle_price_feeder.client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);

				tokio::time::sleep(Duration::from_secs(12)).await;
			}
		},
	);

	// spawn socket rollback emitters
	rollback_emitters.into_iter().for_each(|mut emitter| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-socket-rollback-emitter", emitter.client.get_chain_name())
					.into_boxed_str(),
			),
			Some("rollback"),
			async move {
				loop {
					let report = emitter.run().await;
					let log_msg = format!(
						"rollback emitter({}:{}) stopped: {:?}\nRestarting in 12 seconds...",
						emitter.client.get_chain_name(),
						emitter.client.address(),
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);

					tokio::time::sleep(Duration::from_secs(12)).await;
				}
			},
		)
	});

	// spawn socket relay handlers
	socket_relay_handlers.into_iter().for_each(|mut handler| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-{}-handler", handler.client.get_chain_name(), HandlerType::Socket,)
					.into_boxed_str(),
			),
			Some("handlers"),
			async move {
				loop {
					let report = handler.run().await;
					let log_msg = format!(
						"socket relay handler({}) stopped: {:?}\nRestarting immediately...",
						handler.client.get_chain_name(),
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
				}
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
				loop {
					let report = handler.run().await;
					let log_msg = format!(
						"roundup relay handler({}) stopped: {:?}\nRestarting immediately...",
						handler.client.get_chain_name(),
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
				}
			},
		);
	});

	// spawn roundup emitter
	task_manager.spawn_essential_handle().spawn(
		"roundup-emitter",
		Some("roundup-emitter"),
		async move {
			loop {
				let report = roundup_emitter.run().await;
				let log_msg = format!(
					"roundup emitter({}) stopped: {:?}\nRestarting immediately...",
					roundup_emitter.client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);
			}
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
				let _ = event_manager.bootstrap_0().await;
				loop {
					let report = event_manager.run().await;
					let log_msg = format!(
						"event manager({}) stopped: {:?}\nRestarting immediately...",
						event_manager.client.get_chain_name(),
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
				}
			},
		)
	});

	// spawn bitcoin deps
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-inbound-handler",
		Some("handlers"),
		async move {
			loop {
				let report = inbound.run().await;
				let log_msg = format!(
					"bitcoin inbound handler({}) stopped: {:?}\nRestarting immediately...",
					inbound.bfc_client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);
			}
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-outbound-handler",
		Some("handlers"),
		async move {
			loop {
				let report = outbound.run().await;
				let log_msg = format!(
					"bitcoin outbound handler({}) stopped: {:?}\nRestarting immediately...",
					outbound.bfc_client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);
			}
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-psbt-signer",
		Some("handlers"),
		async move {
			loop {
				let report = psbt_signer.run().await;
				let log_msg = format!(
					"bitcoin psbt signer({}) stopped: {:?}\nRestarting immediately...",
					psbt_signer.client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);
			}
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-public-key-submitter",
		Some("pub-key-submitter"),
		async move {
			loop {
				let report = pub_key_submitter.run().await;
				let log_msg = format!(
					"bitcoin public key submitter({}) stopped: {:?}\nRestarting immediately...",
					pub_key_submitter.client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);
			}
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-rollback-verifier",
		Some("rollback-verifier"),
		async move {
			loop {
				let report = rollback_verifier.run().await;
				let log_msg = format!(
					"bitcoin rollback verifier({}) stopped: {:?}\nRestarting immediately...",
					rollback_verifier.bfc_client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);
			}
		},
	);
	task_manager.spawn_essential_handle().spawn(
		"bitcoin-block-manager",
		Some("block-manager"),
		async move {
			loop {
				let report = block_manager.run().await;
				let log_msg = format!(
					"bitcoin block manager({}) stopped: {:?}\nRestarting immediately...",
					block_manager.bfc_client.address(),
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);
			}
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
	F: TxFiller<AnyNetwork> + WalletProvider<AnyNetwork>,
	P: Provider<T, AnyNetwork>,
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
