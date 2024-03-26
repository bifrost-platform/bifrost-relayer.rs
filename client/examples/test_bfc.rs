extern crate br_client;

use bitcoincore_rpc::bitcoin::key::Secp256k1;
use bitcoincore_rpc::bitcoin::PublicKey;
use br_client::{
	bfc::BfcClient,
	btc::storage::keypair::{KeypairStorage, Network::Bitcoin},
};
use rand::thread_rng;
use subxt::{blocks::ExtrinsicEvents, rpc::RpcClientT, OnlineClient, PolkadotConfig};
use subxt_signer::{sr25519::Keypair, SecretUri};

async fn main_call() -> Result<(), Box<dyn std::error::Error>> {
	let bfc_client = BfcClient::new(
		OnlineClient::<PolkadotConfig>::new().await?,
		Arc::new(EthClient::new(
			WalletManager::from_private_key(system.private_key.as_str(), evm_provider.id)
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
			system.debug_mode.unwrap_or(false),
		)),
		KeypairStorage::new(Testnet),
	)
	.await?;
	OnlineClient::from_rpc_client(Arc::new(rpc_client)).await?;

	let bitcoin_key = Secp256k1::new().generate_keypair(&mut thread_rng());

	let bitcoin29_public_key = PublicKey::new(bitcoin_key.1);

	let events = bfc_client
		.submit_vault_key(
			"0xd6D3f3a35Fab64F69b7885D6162e81B62e44bF58",
			"0x3Cd0A705a2DC65e5b1E1205896BaA2be8A07c6e0",
		)
		.await?;

	println!("Extrinsic 결과: {:?}", events);

	Ok(())
}

#[tokio::main]
async fn main() {
	// main_result().await.unwrap();
	main_call().await.unwrap();
}

// let signer = Keypair::from_uri(
// 	&<SecretUri as std::str::FromStr>::from_str(
// 		"0xcc01ee486e8717dc3911ede9293b767e29ce66f5c987da45887cb61822700117",
// 	)
// 	.unwrap(),
// )
// .unwrap();
