use std::{
	process::{Child, Command},
	sync::Arc,
	time::Duration,
};

use sc_service::{config::PrometheusConfig, Error as ServiceError, TaskManager};

use br_client::{
	bfc::{
		generic::CustomConfig,
		handlers::{BtcOutboundHandler, BtcRegisHandler},
		SubClient,
	},
	btc::{
		block::{BlockManager, EventMessage},
		storage::{
			keypair::KeypairStorage,
			pending_outbound::{PendingOutboundPool, PendingOutboundState},
			vault_set::VaultAddressSet,
		},
	},
	eth::{events::EventManager, traits::Handler, wallet::WalletManager, EthClient},
};

use ethers::{
	prelude::{Http, Provider},
	types::H256,
};

use br_primitives::{
	bootstrap::BootstrapSharedData,
	cli::{Configuration, EVMProvider, HandlerType, RelayerConfig, Result as CliResult},
	constants::errors::{
		INVALID_CONFIG_FILE_PATH, INVALID_CONFIG_FILE_STRUCTURE, INVALID_PRIVATE_KEY,
		INVALID_PROVIDER_URL,
	},
	contracts::btc_registration::RegistrationPoolContract,
	eth::{AggregatorContracts, BootstrapState, ProtocolContracts, ProviderMetadata},
};

use tokio::sync::Barrier;

// use br_cli::cli::runner::build_runtime;
use br_cli::build_runtime;

use bitcoincore_rpc::{bitcoin::Network, jsonrpc, Auth, Client, Error, RpcApi};

use std::str::FromStr;

use subxt::{blocks::ExtrinsicEvents, OnlineClient};

#[cfg(all(test, feature = "btc-registration"))]
mod tests {

	use super::*;

	pub async fn start_node_relayer() -> std::io::Result<Child> {
		let _stop_previous_instance = Command::new("docker")
			.arg("compose")
			.arg("rm")
			.arg("-v")
			.arg("-s")
			.arg("-f")
			.arg("bifrost")
			.status()
			.unwrap();
		let command = Command::new("sh").arg("configs/scripts/run_bifrost_test_docker.sh").spawn();
		command
	}

	async fn test_set_btc_regis_handler(
	) -> (BtcRegisHandler<Http>, SubClient<Http>, TaskManager, BootstrapSharedData) {
		const DEFAULT_GET_LOGS_BATCH_SIZE: u64 = 1;

		const TESTNET_CONFIG_FILE_PATH: &str = "configs/config.testnet.yaml";
		const EXTENDED_MASTER_PRIVATE_KEY: &str =
			"701daf3456e8471c4d37cf1752382b5bbfbbd76ea35065e8ec27df6bf4cd926b";

		let user_config_file =
			std::fs::File::open(TESTNET_CONFIG_FILE_PATH).expect(INVALID_CONFIG_FILE_PATH);
		let user_config: RelayerConfig =
			serde_yaml::from_reader(user_config_file).expect(INVALID_CONFIG_FILE_STRUCTURE);

		let evm_provider: EVMProvider = user_config.evm_providers.first().unwrap().clone();
		let is_native = evm_provider.is_native.unwrap_or(false);

		let provider = Provider::<Http>::try_from(evm_provider.provider.clone())
			.expect(INVALID_PROVIDER_URL)
			.interval(Duration::from_millis(evm_provider.call_interval));

		let eth_client = Arc::new(EthClient::new(
			WalletManager::from_private_key(EXTENDED_MASTER_PRIVATE_KEY, evm_provider.id)
				.expect(INVALID_PRIVATE_KEY),
			Arc::new(provider.clone()),
			ProviderMetadata::new(
				evm_provider.name.clone(),
				evm_provider.id,
				evm_provider.block_confirmations,
				evm_provider.call_interval,
				evm_provider.get_logs_batch_size.unwrap_or(DEFAULT_GET_LOGS_BATCH_SIZE),
				is_native,
			),
			ProtocolContracts::new(
				Arc::new(provider.clone()),
				evm_provider.socket_address.clone(),
				evm_provider.authority_address.clone(),
				evm_provider.relayer_manager_address.clone(),
			),
			AggregatorContracts::new(
				Arc::new(provider),
				evm_provider.chainlink_usdc_usd_address.clone(),
				evm_provider.chainlink_usdt_usd_address.clone(),
				evm_provider.chainlink_dai_usd_address.clone(),
				evm_provider.chainlink_btc_usd_address.clone(),
				evm_provider.chainlink_wbtc_usd_address.clone(),
			),
			true,
		));

		let tokio_handle = build_runtime().unwrap().handle().clone();

		let config =
			Configuration { relayer_config: user_config, tokio_handle: tokio_handle.clone() };

		let task_manager = TaskManager::new(tokio_handle, None).unwrap();

		let bootstrap_shared_data = BootstrapSharedData::new(&config);

		let bfc_client = SubClient::new(
			OnlineClient::<CustomConfig>::new().await.unwrap(),
			eth_client.clone(),
			KeypairStorage::new(Network::Testnet),
		)
		.unwrap();

		let event_manager =
			EventManager::new(eth_client.clone(), Arc::new(bootstrap_shared_data.clone()), false);

		let btc_regis_handler = BtcRegisHandler::new(
			Arc::new(bfc_client.clone()),
			Arc::new(bootstrap_shared_data.clone()),
			event_manager.sender.subscribe(),
			vec![eth_client],
		);
		(btc_regis_handler, bfc_client, task_manager, bootstrap_shared_data)
	}

	#[tokio::test]
	async fn test_btc_request_registration() {
		const REFUND_ADDRESS: &str = "bcrt1qcmnpjjjw78yhyjrxtql6lk7pzpujs3h244p7ae";

		start_node_relayer().await.unwrap();

		let (mut btc_regis_handler, bfc_client, task_manager, bootstrap_shared_data) =
			test_set_btc_regis_handler().await;

		let btc_regis_contract = RegistrationPoolContract::new(
			bfc_client.eth_client.address(),
			bfc_client.eth_client.get_provider(),
		);

		btc_regis_contract.request_vault(REFUND_ADDRESS.into()).call().await.unwrap();

		let bootstrap_states = bootstrap_shared_data.clone().bootstrap_states;

		let mut guard = bootstrap_states.write().await;
		for state in guard.iter_mut() {
			*state = BootstrapState::BootstrapBtcRegis;
		}
		drop(guard);

		btc_regis_handler.run().await;
	}
}
