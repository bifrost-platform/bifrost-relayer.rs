use std::{
	collections::BTreeMap,
	net::{Ipv4Addr, SocketAddr},
	str::FromStr,
	sync::Arc,
	time::Duration,
};

use alloy::{
	network::{AnyNetwork, EthereumWallet, Network as AlloyNetwork},
	primitives::Address,
	providers::{
		Provider, ProviderBuilder, WalletProvider,
		fillers::{ChainIdFiller, GasFiller, TxFiller},
	},
	rpc::client::RpcClient,
	signers::{Signer, aws::AwsSigner, local::PrivateKeySigner},
	transports::http::reqwest::Url,
};
use futures::FutureExt;
use miniscript::bitcoin::Network;
use sc_service::{Error as ServiceError, TaskManager, config::PrometheusConfig};
use tokio::sync::RwLock;

use crate::{
	cli::{LOG_TARGET, SUB_LOG_TARGET},
	service_deps::{
		BtcDeps, FullDeps, HandlerDeps, ManagerDeps, PeriodicDeps, SolDeps, SubstrateDeps,
	},
	verification::assert_configuration_validity,
};
use br_client::{
	btc::{
		handlers::Handler as _,
		storage::keypair::{KeypairStorage, KmsKeypairStorage, PasswordKeypairStorage},
	},
	eth::{EthClient, nonce::RelayerNonceManager, retry::RetryBackoffLayer, traits::Handler as _},
};
use br_periodic::traits::PeriodicWorker;
use br_primitives::{
	bootstrap::BootstrapSharedData,
	cli::{Configuration, HandlerType},
	constants::{
		cli::{DEFAULT_BOOTSTRAP_ROUND_OFFSET, DEFAULT_KEYSTORE_PATH, DEFAULT_PROMETHEUS_PORT},
		errors::{
			INVALID_BIFROST_NATIVENESS, INVALID_BITCOIN_NETWORK, INVALID_PRIVATE_KEY,
			INVALID_PROVIDER_URL, KMS_INITIALIZATION_ERROR, MISSING_BTC_PROVIDER,
		},
		tx::DEFAULT_CALL_RETRIES,
	},
	eth::{AggregatorContracts, ProtocolContracts, ProviderMetadata, Signers},
	substrate::{MigrationSequence, initialize_sub_client},
	utils::sub_display_format,
};

/// Starts the relayer service.
pub async fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	assert_configuration_validity(&config);

	let task_manager = TaskManager::new(config.clone().tokio_handle, None)?;

	let evm_providers = &config.relayer_config.evm_providers;
	let system = &config.relayer_config.system;
	let signer_config = &config.relayer_config.signer_config;
	let keystore_config = &config.relayer_config.keystore_config;

	// Connect to Bifrost's Substrate side first: the runtime's pallets decide which
	// subsystems are enabled.
	let native_provider = evm_providers
		.iter()
		.find(|p| p.is_native.unwrap_or(false))
		.expect(INVALID_BIFROST_NATIVENESS);
	let sub_client =
		initialize_sub_client(native_provider.provider.parse().expect(INVALID_PROVIDER_URL)).await;

	// Bitcoin support follows the runtime: enabled iff its BTC pallets are present, in which
	// case `btc_provider` is mandatory.
	let btc_provider = {
		let metadata = sub_client
			.at_current_block()
			.await
			.expect("Failed to fetch runtime metadata from the Bifrost node")
			.metadata();
		let btc_enabled = ["BtcSocketQueue", "BtcRegistrationPool"]
			.iter()
			.all(|pallet| metadata.pallet_by_name(pallet).is_some());
		if btc_enabled {
			Some(config.relayer_config.btc_provider.as_ref().expect(MISSING_BTC_PROVIDER))
		} else {
			log::info!(
				target: LOG_TARGET,
				"-[{}] ₿ Bitcoin pallets not found in runtime — Bitcoin handlers and workers are disabled",
				sub_display_format(SUB_LOG_TARGET),
			);
			None
		}
	};

	let mut clients = BTreeMap::new();

	// Initialize AWS client once if needed
	let aws_client = if signer_config.iter().any(|s| s.kms_key_id.is_some())
		|| keystore_config.as_ref().and_then(|c| c.kms_key_id.as_ref()).is_some()
	{
		Some(aws_sdk_kms::Client::new(
			&aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await,
		))
	} else {
		None
	};

	let default_address = Arc::new(RwLock::new(Address::default()));
	for evm_provider in evm_providers {
		let mut wallet = EthereumWallet::default();
		let mut signers = Signers::default();
		for s in signer_config {
			if let Some(key_id) = &s.kms_key_id {
				let signer = Arc::new(
					AwsSigner::new(
						aws_client.as_ref().unwrap().clone(),
						key_id.clone(),
						evm_provider.id.into(),
					)
					.await
					.expect(KMS_INITIALIZATION_ERROR),
				);
				wallet.register_signer(signer.clone());
				signers.register_signer(signer);
			} else {
				let mut signer =
					PrivateKeySigner::from_str(&s.private_key.clone().expect(INVALID_PRIVATE_KEY))
						.expect(INVALID_PRIVATE_KEY);
				signer.set_chain_id(evm_provider.id.into());
				let signer = Arc::new(signer);
				wallet.register_signer(signer.clone());
				signers.register_signer(signer);
			}
		}

		let url: Url = evm_provider.provider.clone().parse().expect(INVALID_PROVIDER_URL);
		let is_native = evm_provider.is_native.unwrap_or(false);

		let metadata = ProviderMetadata::new(
			evm_provider.clone(),
			url.clone(),
			if is_native { btc_provider.map(|p| p.id) } else { None },
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
		let nonce_manager = RelayerNonceManager::default();
		let provider = Arc::new(
			ProviderBuilder::<_, _, AnyNetwork>::default()
				.with_nonce_management(nonce_manager.clone())
				.filler(GasFiller::default())
				.filler(ChainIdFiller::new(evm_provider.id.into()))
				.wallet(wallet.clone())
				.connect_client(retry_client.clone()),
		);
		let client = Arc::new(EthClient::new(
			provider.clone(),
			signers.clone(),
			default_address.clone(),
			metadata,
			ProtocolContracts::new(
				is_native,
				btc_provider.is_some(),
				provider.clone(),
				evm_provider.clone(),
			),
			AggregatorContracts::new(
				provider.clone(),
				evm_provider.chainlink_usdc_usd_address.clone(),
				evm_provider.chainlink_usdt_usd_address.clone(),
				evm_provider.chainlink_dai_usd_address.clone(),
				evm_provider.chainlink_btc_usd_address.clone(),
				evm_provider.chainlink_wbtc_usd_address.clone(),
				evm_provider.chainlink_cbbtc_usd_address.clone(),
				evm_provider.chainlink_jpy_usd_address.clone(),
			),
			nonce_manager,
		));

		// initialize default address to selected account
		if is_native {
			client.update_default_address(None).await;
		}

		clients.insert(evm_provider.id, client);
	}

	let bootstrap_shared_data =
		BootstrapSharedData::new(&config, btc_provider.map(|btc_provider| btc_provider.id));

	// The Bitcoin keystore is only needed when Bitcoin support is enabled.
	let keypair_storage = btc_provider.map(|btc_provider| {
		let network = Network::from_core_arg(&btc_provider.chain).expect(INVALID_BITCOIN_NETWORK);
		if let Some(keystore_config) = &keystore_config {
			let keystore_path =
				keystore_config.path.clone().unwrap_or(DEFAULT_KEYSTORE_PATH.to_string());
			if let Some(key_id) = &keystore_config.kms_key_id {
				KeypairStorage::new(KmsKeypairStorage::new(
					keystore_path.clone(),
					network,
					key_id.clone(),
					Arc::new(aws_client.as_ref().unwrap().clone()),
				))
			} else {
				KeypairStorage::new(PasswordKeypairStorage::new(
					keystore_path,
					network,
					keystore_config.password.clone(),
				))
			}
		} else {
			KeypairStorage::new(PasswordKeypairStorage::new(
				DEFAULT_KEYSTORE_PATH.to_string(),
				network,
				None,
			))
		}
	});

	let migration_sequence = Arc::new(RwLock::new(MigrationSequence::Normal));

	let manager_deps = ManagerDeps::new(&config, Arc::new(clients), bootstrap_shared_data.clone());
	let bfc_client = manager_deps.bifrost_client.clone();

	let debug_mode =
		if let Some(system) = system { system.debug_mode.unwrap_or(false) } else { false };
	let substrate_deps = SubstrateDeps::new(bfc_client.clone(), sub_client, &task_manager);
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
	// Build the optional single-cluster Solana wiring BEFORE the handler deps
	// so the SocketRelayHandler can be wired up with its outbound sender.
	// Failure to construct the cluster (e.g. invalid program ID, unreadable fee-payer
	// keypair, unreachable RPC endpoint) is a configuration error and
	// fails fast at boot via the health probe.
	let sol_deps = match &config.relayer_config.sol_provider {
		Some(sol_provider) => Some(
			crate::service_deps::build_sol_deps(
				sol_provider,
				bfc_client.clone(),
				substrate_deps.xt_request_sender.clone(),
				substrate_deps.sub_client.clone(),
				&substrate_deps.sub_rpc_url,
				task_manager.spawn_handle(),
				debug_mode,
				config
					.relayer_config
					.bootstrap_config
					.as_ref()
					.and_then(|bootstrap| bootstrap.round_offset)
					.unwrap_or(DEFAULT_BOOTSTRAP_ROUND_OFFSET),
			)
			.await
			.map_err(|e| ServiceError::Other(format!("failed to build Solana deps: {e}")))?,
		),
		None => {
			log::info!(
				target: LOG_TARGET,
				"-[{}] ◎ sol_provider is not configured — Solana handlers are disabled",
				sub_display_format(SUB_LOG_TARGET),
			);
			None
		},
	};

	// Keep the existing ChainId dispatch table used by the handlers, populated with
	// the configured Solana provider (empty without one).
	let sol_outbound_senders: std::sync::Arc<
		std::collections::BTreeMap<
			alloy::primitives::ChainId,
			br_client::sol::handlers::outbound::SolOutboundSender,
		>,
	> = std::sync::Arc::new(
		sol_deps
			.iter()
			.map(|sol_deps| (sol_deps.client.chain_id, sol_deps.outbound_sender.clone()))
			.collect(),
	);

	// Parallel map of `SolClient`s keyed by the same `ChainId`. The
	// RoundupRelayHandler uses these to probe `socket_config.latest_round_id`
	// in the Solana dispatch + retry paths. Shares the same `Arc<RpcClient>`
	// the outbound workers use, so no extra connection is opened.
	let sol_clients: std::sync::Arc<
		std::collections::BTreeMap<alloy::primitives::ChainId, br_client::sol::client::SolClient>,
	> = std::sync::Arc::new(
		sol_deps
			.iter()
			.map(|sol_deps| (sol_deps.client.chain_id, sol_deps.client.clone()))
			.collect(),
	);

	let handler_deps = HandlerDeps::new(
		&config,
		&manager_deps,
		&substrate_deps,
		bootstrap_shared_data.clone(),
		bfc_client.clone(),
		periodic_deps.rollback_senders.clone(),
		sol_clients,
		sol_outbound_senders,
		&task_manager,
		debug_mode,
	)
	.await;
	let btc_deps =
		btc_provider
			.zip(keypair_storage.clone())
			.map(|(btc_provider, keypair_storage)| {
				BtcDeps::new(
					btc_provider,
					keypair_storage,
					bootstrap_shared_data.clone(),
					&substrate_deps,
					migration_sequence.clone(),
					bfc_client.clone(),
					&task_manager,
					debug_mode,
				)
			});

	print_relay_targets(&manager_deps).await;

	Ok(spawn_relayer_tasks(
		task_manager,
		FullDeps { manager_deps, periodic_deps, handler_deps, substrate_deps, btc_deps, sol_deps },
		&config,
	))
}

/// Spawn relayer service tasks by the `TaskManager`.
fn spawn_relayer_tasks<F, P, N: AlloyNetwork>(
	task_manager: TaskManager,
	deps: FullDeps<F, P, N>,
	config: &Configuration,
) -> TaskManager
where
	F: TxFiller<N> + WalletProvider<N> + 'static,
	P: Provider<N> + 'static,
{
	let prometheus_config = &config.relayer_config.prometheus_config;

	let FullDeps { manager_deps, periodic_deps, handler_deps, substrate_deps, btc_deps, sol_deps } =
		deps;

	let ManagerDeps { event_managers, .. } = manager_deps;
	let PeriodicDeps {
		mut heartbeat_sender,
		mut oracle_price_feeder,
		mut price_deviation_checker,
		mut roundup_emitter,
		rollback_emitters,
		keypair_migrator,
		presubmitter,
		..
	} = periodic_deps;
	let HandlerDeps {
		socket_relay_handlers,
		socket_queue_pollers,
		roundup_relay_handlers,
		socket_onflight_handler,
	} = handler_deps;
	let SubstrateDeps { mut unsigned_tx_manager, .. } = substrate_deps;
	// spawn migration detector and public key presubmitter (Bitcoin only)
	if let (Some(mut keypair_migrator), Some(mut presubmitter)) = (keypair_migrator, presubmitter) {
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
						presubmitter.bfc_client.address().await,
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);

					tokio::time::sleep(Duration::from_secs(12)).await;
				}
			},
		);
	}

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
					heartbeat_sender.client.address().await,
					report
				);
				log::error!("{log_msg}");
				sentry::capture_message(&log_msg, sentry::Level::Error);

				tokio::time::sleep(Duration::from_secs(12)).await;
			}
		});

	// // spawn oracle price feeder
	// task_manager.spawn_essential_handle().spawn(
	// 	Box::leak(
	// 		format!("{}-oracle-price-feeder", oracle_price_feeder.client.get_chain_name())
	// 			.into_boxed_str(),
	// 	),
	// 	Some("oracle"),
	// 	async move {
	// 		loop {
	// 			let report = oracle_price_feeder.run().await;
	// 			let log_msg = format!(
	// 				"oracle price feeder({}:{}) stopped: {:?}\nRestarting in 12 seconds...",
	// 				oracle_price_feeder.client.get_chain_name(),
	// 				oracle_price_feeder.client.address().await,
	// 				report
	// 			);
	// 			log::error!("{log_msg}");
	// 			sentry::capture_message(&log_msg, sentry::Level::Error);

	// 			tokio::time::sleep(Duration::from_secs(12)).await;
	// 		}
	// 	},
	// );

	// // spawn price deviation checker
	// task_manager.spawn_essential_handle().spawn(
	// 	Box::leak(
	// 		format!("{}-price-deviation-checker", price_deviation_checker.client.get_chain_name())
	// 			.into_boxed_str(),
	// 	),
	// 	Some("oracle"),
	// 	async move {
	// 		loop {
	// 			let report = price_deviation_checker.run().await;
	// 			let log_msg = format!(
	// 				"price deviation checker({}:{}) stopped: {:?}\nRestarting in 12 seconds...",
	// 				price_deviation_checker.client.get_chain_name(),
	// 				price_deviation_checker.client.address().await,
	// 				report
	// 			);
	// 			log::error!("{log_msg}");
	// 			sentry::capture_message(&log_msg, sentry::Level::Error);

	// 			tokio::time::sleep(Duration::from_secs(12)).await;
	// 		}
	// 	},
	// );

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
						emitter.client.address().await,
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
				format!("{}-{:?}-handler", handler.client.get_chain_name(), HandlerType::Socket)
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

	// spawn socket onflight handler (single instance for Bifrost chain)
	if let Some(mut handler) = socket_onflight_handler {
		task_manager.spawn_essential_handle().spawn(
			"socket-onflight-handler",
			Some("handlers"),
			async move {
				loop {
					let report = handler.run().await;
					let log_msg = format!(
						"socket onflight handler stopped: {:?}\nRestarting in 12 seconds...",
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);

					tokio::time::sleep(Duration::from_secs(12)).await;
				}
			},
		);
	}

	// spawn socket queue pollers
	socket_queue_pollers.into_iter().for_each(|mut handler| {
		task_manager.spawn_essential_handle().spawn(
			Box::leak(
				format!("{}-socket-queue-poller", handler.client.get_chain_name()).into_boxed_str(),
			),
			Some("handlers"),
			async move {
				loop {
					let report = handler.run().await;
					let log_msg = format!(
						"socket queue poller({}) stopped: {:?}\nRestarting immediately...",
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
				format!("{}-{:?}-handler", handler.client.get_chain_name(), HandlerType::Roundup)
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
					roundup_emitter.client.address().await,
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
				event_manager.bootstrap_0().await;
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
	if let Some(BtcDeps {
		mut outbound,
		mut inbound,
		mut block_manager,
		mut psbt_signer,
		mut psbt_broadcaster,
		mut pub_key_submitter,
		mut rollback_verifier,
		mut fee_rate_feeder,
	}) = btc_deps
	{
		task_manager.spawn_essential_handle().spawn(
			"bitcoin-inbound-handler",
			Some("handlers"),
			async move {
				loop {
					let report = inbound.run().await;
					let log_msg = format!(
						"bitcoin inbound handler({}) stopped: {:?}\nRestarting immediately...",
						inbound.bfc_client.address().await,
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
						outbound.bfc_client.address().await,
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
						psbt_signer.client.address().await,
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
				}
			},
		);
		task_manager.spawn_essential_handle().spawn(
			"bitcoin-psbt-broadcaster",
			Some("psbt-broadcaster"),
			async move {
				loop {
					let report = psbt_broadcaster.run().await;
					let log_msg = format!(
						"bitcoin psbt broadcaster({}) stopped: {:?}\nRestarting immediately...",
						psbt_broadcaster.bfc_client.address().await,
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
						pub_key_submitter.client.address().await,
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
						rollback_verifier.bfc_client.address().await,
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
				}
			},
		);
		task_manager.spawn_essential_handle().spawn(
			"bitcoin-fee-rate-feeder",
			Some("fee-rate-feeder"),
			async move {
				loop {
					let report = fee_rate_feeder.run().await;
					let log_msg = format!(
						"bitcoin fee rate feeder({}) stopped: {:?}\nRestarting immediately...",
						fee_rate_feeder.bfc_client.address().await,
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
				// Bootstrap failures are fatal: exiting is preferable to idling while every chain
				// waits on this one at the bootstrap barrier.
				block_manager
					.bootstrap_0()
					.await
					.expect("bitcoin block manager bootstrap failed");
				// After bootstrap, a Bitcoin node outage must not take down the other chains, so
				// `run()` errors are retried in place.
				loop {
					let report = block_manager.run().await;
					let log_msg = format!(
						"bitcoin block manager({}) stopped: {:?}\nRestarting in 12 seconds...",
						block_manager.bfc_client.address().await,
						report
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);

					tokio::time::sleep(Duration::from_secs(12)).await;
				}
			},
		);
	}

	// Three workers for the optional `sol_provider`: slot-manager / outbound /
	// queue-poller. Inbound ingestion is the queue poller's job via
	// `cccp-relay-queue`; there is no separate inbound handler.
	if let Some(SolDeps {
		client,
		bootstrap_replay_slots,
		mut slot_manager,
		mut outbound,
		outbound_sender: _,
		mut queue_poller,
	}) = sol_deps
	{
		let cluster_name = client.get_chain_name();

		let slot_label = format!("solana-slot-manager-{}", cluster_name);
		let slot_label: &'static str = Box::leak(slot_label.into_boxed_str());
		task_manager.spawn_essential_handle().spawn(
			slot_label,
			Some("solana-slot-manager"),
			async move {
				// This relayer is stateless across process restarts, so the
				// finalized bootstrap window is the recovery source of truth.
				// Never enter live polling after a failed history fetch:
				// doing so could permanently skip an event that falls behind
				// the in-memory cursor.
				loop {
					match slot_manager.bootstrap_catchup(bootstrap_replay_slots).await {
						Ok(()) => break,
						Err(err) => {
							log::error!(
								"solana slot manager bootstrap catch-up failed: {err:?}; \
								 retrying in 12 seconds before live polling",
							);
							sentry::capture_message(
								&format!("solana bootstrap catch-up failed: {err:?}"),
								sentry::Level::Error,
							);
							tokio::time::sleep(Duration::from_secs(12)).await;
						},
					}
				}

				loop {
					let report = slot_manager.run().await;
					let log_msg = format!(
						"solana slot manager stopped: {report:?}\nRestarting in 12 seconds...",
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
					tokio::time::sleep(Duration::from_secs(12)).await;
				}
			},
		);

		let outbound_label = format!("solana-outbound-handler-{}", cluster_name);
		let outbound_label: &'static str = Box::leak(outbound_label.into_boxed_str());
		task_manager.spawn_essential_handle().spawn(
			outbound_label,
			Some("solana-outbound"),
			async move {
				loop {
					let report = outbound.run().await;
					let log_msg = format!(
						"solana outbound handler stopped: {report:?}\nRestarting in 12 seconds...",
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
					tokio::time::sleep(Duration::from_secs(12)).await;
				}
			},
		);

		let queue_poller_label = format!("solana-queue-poller-{}", cluster_name);
		let queue_poller_label: &'static str = Box::leak(queue_poller_label.into_boxed_str());
		task_manager.spawn_essential_handle().spawn(
			queue_poller_label,
			Some("solana-queue-poller"),
			async move {
				loop {
					let report = queue_poller.run().await;
					let log_msg = format!(
						"solana queue poller stopped: {report:?}\nRestarting in 12 seconds...",
					);
					log::error!("{log_msg}");
					sentry::capture_message(&log_msg, sentry::Level::Error);
					tokio::time::sleep(Duration::from_secs(12)).await;
				}
			},
		);
	}

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
async fn print_relay_targets<F, P, N: AlloyNetwork>(manager_deps: &ManagerDeps<F, P, N>)
where
	F: TxFiller<N> + WalletProvider<N>,
	P: Provider<N>,
{
	log::info!(
		target: LOG_TARGET,
		"-[{}] 👤 Provided signers: {:?}",
		sub_display_format(SUB_LOG_TARGET),
		manager_deps.bifrost_client.signers()
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
