use cc_cli::Configuration;
use cccp_client::eth::{EthClient, EventDetector, TransactionManager};
use cccp_primitives::eth::{
	bfc_testnet::{BFC_CALL_INTERVAL_MS, BFC_SOCKET_CONTRACT_ADDRESS},
	bsc_testnet::{BSC_CALL_INTERVAL_MS, BSC_SOCKET_CONTRACT_ADDRESS},
	eth_testnet::{ETH_CALL_INTERVAL_MS, ETH_SOCKET_CONTRACT_ADDRESS},
	polygon_testnet::{POLYGON_CALL_INTERVAL_MS, POLYGON_SOCKET_CONTRACT_ADDRESS},
	EthClientConfiguration,
};
use sc_service::{Error as ServiceError, TaskManager};
use std::{str::FromStr, sync::Arc};

use ethers::{
	providers::{Http, Provider},
	types::{Address, H160},
};

pub fn relay(config: Configuration) -> Result<TaskManager, ServiceError> {
	new_relay_base(config).map(|RelayBase { task_manager, .. }| task_manager)
}

pub fn new_relay_base(config: Configuration) -> Result<RelayBase, ServiceError> {
	let task_manager = TaskManager::new(config.tokio_handle, None)?;

	// TODO: Workers tasks should spawn from here.
	// let client =
	// Arc::new(Provider::<Http>::try_from(&config.private_config.eth_provider).unwrap());
	// let socket_address =
	// "0x4A31FfeAc276CC5e508cAC0568d932d398C4DD84".parse::<Address>().unwrap();

	// let mut evm_manager = EVMTransactionManager::new(client, socket_address).unwrap();

	// task_manager.spawn_essential_handle().spawn_blocking(
	// 	"bfc-tx-manager",
	// 	Some("txmanager"),
	// 	async move { evm_manager.run().await },
	// );

	// TODO: add event detectors for every evm-chain
	// TODO: add --chain cli option (dev, testnet, mainnet)

	let bfc_client = Arc::new(EthClient::new(
		Provider::<Http>::try_from(&config.private_config.bfc_provider).unwrap(),
		EthClientConfiguration {
			name: "bfc-testnet".to_string(),
			call_interval: BFC_CALL_INTERVAL_MS,
			socket_address: H160::from_str(BFC_SOCKET_CONTRACT_ADDRESS).unwrap(),
		},
	));
	let bfc_event_detector = EventDetector::new(Arc::clone(&bfc_client));
	let bfc_tx_manager = TransactionManager::new(Arc::clone(&bfc_client));

	// let eth_client = EthClient::new(
	// 	Arc::new(Provider::<Http>::try_from(&config.private_config.eth_provider).unwrap()),
	// 	EthClientConfiguration {
	// 		name: "eth-testnet".to_string(),
	// 		call_interval: ETH_CALL_INTERVAL_MS,
	// 		socket_address: H160::from_str(ETH_SOCKET_CONTRACT_ADDRESS).unwrap(),
	// 	},
	// );
	// let mut eth_event_detector = EventDetector::new(Arc::new(eth_client));

	// let bsc_client = EthClient::new(
	// 	Arc::new(Provider::<Http>::try_from(&config.private_config.bsc_provider).unwrap()),
	// 	EthClientConfiguration {
	// 		name: "bsc-testnet".to_string(),
	// 		call_interval: BSC_CALL_INTERVAL_MS,
	// 		socket_address: H160::from_str(BSC_SOCKET_CONTRACT_ADDRESS).unwrap(),
	// 	},
	// );
	// let mut bsc_event_detector = EventDetector::new(Arc::new(bsc_client));

	// let polygon_client = EthClient::new(
	// 	Arc::new(Provider::<Http>::try_from(&config.private_config.polygon_provider).unwrap()),
	// 	EthClientConfiguration {
	// 		name: "polygon-testnet".to_string(),
	// 		call_interval: POLYGON_CALL_INTERVAL_MS,
	// 		socket_address: H160::from_str(POLYGON_SOCKET_CONTRACT_ADDRESS).unwrap(),
	// 	},
	// );
	// let mut polygon_event_detector = EventDetector::new(Arc::new(polygon_client));

	task_manager
		.spawn_essential_handle()
		.spawn("bfc-event-detector", Some("events"), async move { bfc_event_detector.run().await });
	task_manager.spawn_essential_handle().spawn(
		"bfc-tx-manager",
		Some("transactions"),
		async move { bfc_tx_manager.run().await },
	);

	// task_manager.spawn_essential_handle().spawn_blocking(
	// 	"eth-event-detector",
	// 	Some("events"),
	// 	async move { eth_event_detector.run().await },
	// );
	// task_manager.spawn_essential_handle().spawn_blocking(
	// 	"bsc-event-detector",
	// 	Some("events"),
	// 	async move { bsc_event_detector.run().await },
	// );
	// task_manager.spawn_essential_handle().spawn_blocking(
	// 	"polygon-event-detector",
	// 	Some("events"),
	// 	async move { polygon_event_detector.run().await },
	// );

	Ok(RelayBase { task_manager })
}

pub struct RelayBase {
	/// The task manager of the node.
	pub task_manager: TaskManager,
}
