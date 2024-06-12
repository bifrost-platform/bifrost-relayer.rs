use base64::{engine::general_purpose, Engine as _};
use br_client::btc::storage::keypair::KeypairStorage;
use br_client::eth::wallet::WalletManager;
use br_client::eth::EthClient;
use br_primitives::{
	cli::{BTCProvider, EVMProvider, RelayerConfig},
	constants::errors::{
		INVALID_CONFIG_FILE_PATH, INVALID_CONFIG_FILE_STRUCTURE, INVALID_PRIVATE_KEY,
		INVALID_PROVIDER_URL,
	},
	contracts::{bitcoin_socket::UnifiedBtcContract, vault::VaultContract},
	eth::{AggregatorContracts, ProtocolContracts, ProviderMetadata},
	substrate::{bifrost_runtime, AccountId20, CustomConfig, Public, VaultKeySubmission},
	utils::convert_ethers_to_ecdsa_signature,
};

use br_primitives::contracts::vault::{Instruction, TaskParams, UserRequest};

use tokio::io::AsyncWriteExt;

use ethers::{
	core::rand,
	middleware::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
	providers::{Http, Middleware, Provider},
	signers::{LocalWallet, Signer},
	types::{
		transaction::eip2718::TypedTransaction, Address, Bytes, TransactionRequest, H160, U256,
	},
	utils::hex,
};
use miniscript::bitcoin::{
	address::NetworkUnchecked, Address as BtcAddress, Amount, Network, Psbt, PublicKey,
};
use reqwest::Client;
use serde::{Deserialize, Serialize}; // Corrected import path for sp_keyring
use std::{
	collections::HashMap,
	io::{BufReader, Read},
	path::Path,
};
use std::{fs::File, sync::Arc};
use std::{
	str::FromStr,
	// thread::sleep
};
use subxt::{blocks::ExtrinsicEvents, OnlineClient};
// use subxt::OnlineClient;
// use tokio::runtime::Runtime;
use bitcoincore_rpc::{json::WalletCreateFundedPsbtOptions, Auth, Client as BitcoinClient, RpcApi};
use tokio::time::{sleep, Duration};

#[derive(Serialize, Deserialize, Debug)]
pub struct WalletEntry(Vec<(String, WalletDetails)>);

impl WalletEntry {
	pub fn get_details(&self) -> &[(String, WalletDetails)] {
		&self.0
	}

	pub fn get_details_mut(&mut self) -> &mut [(String, WalletDetails)] {
		&mut self.0
	}
}

#[derive(Serialize, Deserialize, Debug)]
pub struct WalletDetails {
	pub pub_key: H160,
	pub priv_key: String,
	pub refund_address: String,
	pub vault_address: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct CreateWalletParams {
	wallet_name: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcRequest<T> {
	jsonrpc: String,
	id: String,
	method: String,
	params: T,
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcResponse {
	result: Option<serde_json::Value>,
	error: Option<serde_json::Value>,
	id: String,
}

const TOKEN_ID_0: &str = "0x00000003000000020000bfc04bb70f390bfe7181179534795238784c3344c365";
const TOKEN_ID_1: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const CHAIN_ID: &str = "0x00002712";
const METHOD_ID: &str = "0x03020301000000000000000000000000";
const SAT_DECIMALS: f64 = 100_000_000.0;
const TIME_SLEEP: u64 = 10;
const UNIFIED_BTC_ADDRESS: &str = "0x4bb70f390bfe7181179534795238784c3344c365";
const DEFAULT_WALLET_NAME: &str = "default";

pub async fn test_submit_vault_key(
	bfc_client: Arc<EthClient<Http>>,
	sub_client: OnlineClient<CustomConfig>,
	pub_key: PublicKey,
	who: H160, // user eth address
) -> Result<ExtrinsicEvents<CustomConfig>, Box<dyn std::error::Error>> {
	let mut converted_pub_key = [0u8; 33];
	converted_pub_key.copy_from_slice(&pub_key.to_bytes());
	// submit public key
	let msg = VaultKeySubmission {
		authority_id: AccountId20(bfc_client.address().0),
		who: AccountId20(who.0),
		pub_key: Public(converted_pub_key),
	};
	let message = array_bytes::bytes2hex("0x", converted_pub_key);
	let signature =
		convert_ethers_to_ecdsa_signature(bfc_client.wallet.sign_message(&message.as_bytes()));

	let payload = bifrost_runtime::tx().btc_registration_pool().submit_vault_key(msg, signature);

	let events: ExtrinsicEvents<CustomConfig> = sub_client
		.tx()
		.create_unsigned(&payload)?
		.submit_and_watch()
		.await?
		.wait_for_finalized_success()
		.await?;

	Ok(events)
}

pub async fn test_create_keypair(keyapair_path: &str, keyapair_secreate: &str) -> KeypairStorage {
	// let keypair_storage =
	// 	KeypairStorage::new("../localkeystore_test", Some("test".to_string()), Network::Regtest);

	let keypair_storage =
		KeypairStorage::new(keyapair_path, Some(keyapair_secreate.to_string()), Network::Regtest);

	keypair_storage
}

pub async fn get_btc_client(btc_provider: BTCProvider) -> BitcoinClient {
	let auth = Auth::UserPass(
		btc_provider.username.clone().unwrap(),
		btc_provider.password.clone().unwrap(),
	);

	println!("btc_url: {:?}", btc_provider.provider.as_str());

	BitcoinClient::new(btc_provider.provider.as_str(), auth).expect(INVALID_PROVIDER_URL)
}

pub async fn test_get_vault_contract(
	bfc_client: Arc<EthClient<Http>>,
	vault_contract_address: &str,
) -> VaultContract<Provider<Http>> {
	let vault_contract: VaultContract<Provider<Http>> = VaultContract::new(
		H160::from_str(vault_contract_address).unwrap(),
		bfc_client.get_provider().clone(),
	);

	vault_contract
}

pub async fn read_wallet_details(file_path: &str) -> Vec<WalletDetails> {
	// Open the file in read-only mode with buffer.
	let file = File::open(file_path).expect("Failed to open file");
	let reader = BufReader::new(file);

	// Deserialize the JSON into a vector of WalletDetails.
	let wallet_details: Vec<WalletDetails> =
		serde_json::from_reader(reader).expect("Failed to parse JSON");
	wallet_details
}

pub async fn get_unified_btc(
	bfc_client: Arc<EthClient<Http>>,
	unified_btc_address: &str,
) -> UnifiedBtcContract<Provider<Http>> {
	let unified_btc: UnifiedBtcContract<Provider<Http>> = UnifiedBtcContract::new(
		H160::from_str(unified_btc_address).unwrap(),
		bfc_client.get_provider().clone(),
	);

	unified_btc
}

pub async fn get_btc_wallet_balance(
	user_name: &str,
	password: &str,
	btc_url: &str,
	wallet_name: &str,
) -> Result<Amount, Box<dyn std::error::Error>> {
	let client = Client::new();

	let get_new_address_request = JsonRpcRequest {
		jsonrpc: "1.0".to_string(),
		id: "curltest".to_string(),
		method: "getbalance".to_string(),
		params: Vec::<String>::new(),
	};

	let new_wallet_url = format!("{}/wallet/{}", btc_url, wallet_name);

	let response = client
		.post(&new_wallet_url)
		.basic_auth(user_name, Some(password))
		.json(&get_new_address_request)
		.send()
		.await?;

	let response_text = response.text().await?;

	let balance_json: JsonRpcResponse = serde_json::from_str(&response_text).unwrap();

	let amount = Amount::from_btc(balance_json.result.unwrap().to_string().parse::<f64>()?)?;
	Ok(amount)
}

pub async fn send_btc_transaction(
	user_name: &str,
	password: &str,
	btc_url: &str,
	vault_address: BtcAddress,
	wallet_name: &str,
	amount: &str,
	metadata: Option<&str>, // Optional inscription data
) {
	let client = Client::new();

	// Create the base transaction request
	let mut params = vec![serde_json::json!(vault_address.to_string()), serde_json::json!(amount)];

	// If there is an inscription, add an additional output with OP_RETURN
	if let Some(inscription_data) = metadata {
		let op_return_script = format!("6a{}", hex::encode(inscription_data));
		params.push(serde_json::json!({
			"data": op_return_script
		}));
	}

	let get_new_transfer_request = JsonRpcRequest {
		jsonrpc: "1.0".to_string(),
		id: "curltest".to_string(),
		method: "sendtoaddress".to_string(),
		params: serde_json::json!(params),
	};

	let new_wallet_url = format!("{}/wallet/{}", btc_url, wallet_name);

	let create_new_transfer_response = client
		.post(&new_wallet_url)
		.basic_auth(user_name, Some(password))
		.json(&get_new_transfer_request)
		.send()
		.await
		.unwrap();

	let create_new_transfer_response_text = create_new_transfer_response.text().await.unwrap();
	println!("Create transfer response: {}", create_new_transfer_response_text);
}

pub async fn test_create_new_wallet(
	user_name: &str,
	password: &str,
	btc_url: &str,
	wallet_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
	let client = Client::new();

	let create_wallet_request = JsonRpcRequest {
		jsonrpc: "1.0".to_string(),
		id: "curltest".to_string(),
		method: "createwallet".to_string(),
		params: CreateWalletParams { wallet_name: wallet_name.to_string().clone() },
	};

	let create_wallet_response = client
		.post(btc_url)
		.basic_auth(user_name, Some(password))
		.json(&create_wallet_request)
		.send()
		.await
		.unwrap();

	let create_wallet_response_text = create_wallet_response.text().await.unwrap();
	println!("Create wallet response: {}", create_wallet_response_text);

	let create_wallet_response_json: JsonRpcResponse =
		serde_json::from_str(&create_wallet_response_text).unwrap();

	if let Some(result) = create_wallet_response_json.result {
		println!("Wallet created successfully: {:?}", result);
		Ok(())
	} else if let Some(error) = create_wallet_response_json.error {
		println!("Error creating wallet: {:?}", error);
		return Err(Box::new(std::io::Error::new(
			std::io::ErrorKind::Other,
			"Wallet creation failed",
		)));
	} else {
		return Err(Box::new(std::io::Error::new(
			std::io::ErrorKind::Other,
			"Unexpected response format",
		)));
	}
}

pub async fn test_set_btc_wallet(
	user_name: &str,
	password: &str,
	btc_url: &str,
	wallet_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
	let client = Client::new();

	let get_new_address_request = JsonRpcRequest {
		jsonrpc: "1.0".to_string(),
		id: "curltest".to_string(),
		method: "getnewaddress".to_string(),
		params: Vec::<String>::new(),
	};

	let new_wallet_url = format!("{}/wallet/{}", btc_url, wallet_name);

	let get_new_address_response = client
		.post(&new_wallet_url)
		.basic_auth(user_name, Some(password))
		.json(&get_new_address_request)
		.send()
		.await?;

	let status = get_new_address_response.status();
	let get_new_address_response_text = get_new_address_response.text().await?;

	if get_new_address_response_text.is_empty() {
		println!("Error: Received empty response for getnewaddress request");
		return Err(Box::new(std::io::Error::new(
			std::io::ErrorKind::Other,
			format!("Received empty response for getnewaddress request with status: {}", status),
		)));
	}

	let get_new_address_response_json: JsonRpcResponse =
		serde_json::from_str(&get_new_address_response_text)?;
	if let Some(result) = get_new_address_response_json.result {
		Ok(result.to_string())
	} else if let Some(error) = get_new_address_response_json.error {
		Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "Getting new address failed")))
	} else {
		Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "Unexpected response format")))
	}
}

pub async fn test_set_sub_client(sub_url: &str) -> OnlineClient<CustomConfig> {
	let sub_client = OnlineClient::<CustomConfig>::from_url(sub_url)
		.await
		.expect(INVALID_PROVIDER_URL);

	sub_client
}

pub async fn test_set_bfc_client(priv_key: &str) -> (Arc<EthClient<Http>>, BTCProvider) {
	let current_working_path: String = std::env::current_dir().unwrap().display().to_string();

	let base_path = Path::new(&current_working_path).parent().unwrap();
	let testnet_config_file_path_raw = base_path.join("configs/config.testnet.yaml");
	let testnet_config_file_path = testnet_config_file_path_raw.to_str().unwrap();

	const DEFAULT_GET_LOGS_BATCH_SIZE: u64 = 1;

	let user_config_file =
		std::fs::File::open(testnet_config_file_path).expect(INVALID_CONFIG_FILE_PATH);
	let user_config: RelayerConfig =
		serde_yaml::from_reader(user_config_file).expect(INVALID_CONFIG_FILE_STRUCTURE);

	let evm_provider: EVMProvider = user_config.evm_providers.first().unwrap().clone();
	let btc_provider = user_config.btc_provider;

	let is_native = evm_provider.is_native.unwrap_or(false);

	let provider = Provider::<Http>::try_from(evm_provider.provider.clone())
		.expect(INVALID_PROVIDER_URL)
		.interval(Duration::from_millis(evm_provider.call_interval));

	let bfc_client = Arc::new(EthClient::new(
		WalletManager::from_private_key(priv_key, evm_provider.id).expect(INVALID_PRIVATE_KEY),
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
			Arc::new(provider.clone()),
			evm_provider.chainlink_usdc_usd_address.clone(),
			evm_provider.chainlink_usdt_usd_address.clone(),
			evm_provider.chainlink_dai_usd_address.clone(),
			evm_provider.chainlink_btc_usd_address.clone(),
			evm_provider.chainlink_wbtc_usd_address.clone(),
		),
		true,
	));

	(bfc_client, btc_provider)
}

pub async fn create_new_account() -> Result<(LocalWallet, Address), Box<dyn std::error::Error>> {
	// Generate a new keypair
	let wallet = LocalWallet::new(&mut rand::thread_rng());

	// Get the address
	let address = wallet.address();

	Ok((wallet, address))
}

// pub async fn get_btc_wallet_balance(
// 	btc_client: BitcoinClient,
// ) -> Result<Amount, Box<dyn std::error::Error>> {
// 	let balance = btc_client.get_balance(None, None).await?;

// 	Ok(balance)
// }

pub async fn build_psbt(
	request: HashMap<BtcAddress<NetworkUnchecked>, Amount>,
	system_vault: &str,
	btc_client: BitcoinClient,
) -> Vec<u8> {
	let mut outputs: HashMap<String, Amount> = HashMap::new();

	for (address, amount) in request.iter() {
		outputs.insert(address.assume_checked_ref().to_string(), *amount);
	}

	let mut option = WalletCreateFundedPsbtOptions::default();
	option.add_inputs = true.into();
	option.change_address = BtcAddress::from_str(&system_vault).unwrap().into();
	option.change_position = 0.into();

	let psbt = btc_client
		.wallet_create_funded_psbt(&vec![], &outputs, None, option.into(), true.into())
		.await
		.unwrap()
		.psbt;

	general_purpose::STANDARD.decode(&psbt).unwrap()
}

pub async fn transfer_fund(
	new_address: &str,
	amount: u128,
	alice_priv_key: &str,
) -> Result<(Option<ethers::types::TransactionReceipt>), Box<dyn std::error::Error>> {
	// Convert Alice's private key to a LocalWallet
	let new_wallet: LocalWallet =
		new_address.parse().map_err(|_| "Invalid new wallet private key")?;

	let (bfc_client, btc_provider) = test_set_bfc_client(alice_priv_key).await;

	let middleware = bfc_client
		.get_provider()
		.clone()
		.wrap_into(|p| SignerMiddleware::new(p, bfc_client.wallet.signer.clone()))
		.wrap_into(|p| NonceManagerMiddleware::new(p, bfc_client.address()));

	// Create a transaction request
	let request = TransactionRequest::pay(new_wallet.address(), U256::from(amount))
		.from(bfc_client.address());

	let nonce = middleware.get_transaction_count(bfc_client.address(), None).await.unwrap();
	let request = request.nonce(nonce);

	let request = request
		.clone()
		.gas(middleware.estimate_gas(&TypedTransaction::Legacy(request), None).await.unwrap());

	let pending_tx = middleware.send_transaction(request, None).await.unwrap();

	let receipt: Option<ethers::types::TransactionReceipt> = pending_tx.await?;

	Ok(receipt)
}

pub async fn approve_contract_tx(
	bfc_client: Arc<EthClient<Http>>,
	amount_eth: U256,
	contract_address: &str,
) {
	let unified_btc = get_unified_btc(bfc_client.clone(), UNIFIED_BTC_ADDRESS).await;
	let contract = test_get_vault_contract(bfc_client.clone(), contract_address).await;

	let middleware = bfc_client
		.get_provider()
		.clone()
		.wrap_into(|p| SignerMiddleware::new(p, bfc_client.wallet.signer.clone()))
		.wrap_into(|p| NonceManagerMiddleware::new(p, bfc_client.address()));

	let approve = TransactionRequest::new()
		.data(unified_btc.approve(contract.address(), amount_eth).calldata().unwrap())
		.to(unified_btc.address())
		.from(bfc_client.address());

	let approve = approve
		.clone()
		.gas(middleware.estimate_gas(&TypedTransaction::Legacy(approve), None).await.unwrap());

	let approve_tx = middleware.send_transaction(approve, None).await;
	match approve_tx {
		Ok(pending_tx) => match pending_tx.await {
			Ok(receipt) => {
				println!(
					"⚡️ Approved UnifiedBTC: {:?}:{:?}",
					amount_eth,
					receipt.unwrap().transaction_hash
				)
			},
			Err(error) => {
				panic!("approve failed: {:?}", error);
			},
		},
		Err(error) => {
			panic!("approve failed: {:?}", error);
		},
	}
}

pub async fn check_registration(bfc_client: Arc<EthClient<Http>>, refund_address: &str) -> String {
	let registration_pool = bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

	let vault_address = bfc_client
		.contract_call(
			registration_pool.vault_address(bfc_client.address()),
			"registration_pool.vault_address",
		)
		.await;

	println!("vault_address: {:?}", vault_address);

	let registration_info = bfc_client
		.contract_call(
			registration_pool.registration_info(bfc_client.address()),
			"registration_pool.registration_info",
		)
		.await;

	assert_eq!(registration_info.1, refund_address);
	assert_eq!(registration_info.2, vault_address);

	vault_address
}

pub async fn registration(refund_address: &str, bfc_client: Arc<EthClient<Http>>) {
	let middleware = bfc_client
		.get_provider()
		.clone()
		.wrap_into(|p| SignerMiddleware::new(p, bfc_client.wallet.signer.clone()))
		.wrap_into(|p| NonceManagerMiddleware::new(p, bfc_client.address()));

	println!("refund_address: {:?}", refund_address);

	let registration_pool = bfc_client.protocol_contracts.registration_pool.as_ref().unwrap();

	let calldata = registration_pool.request_vault(refund_address.to_string()).calldata().unwrap();

	let request = TransactionRequest::default().data(calldata).to(registration_pool.address());

	let nonce = middleware.get_transaction_count(bfc_client.address(), None).await.unwrap();
	let request = request.nonce(nonce);

	let request = request
		.clone()
		.gas(middleware.estimate_gas(&TypedTransaction::Legacy(request), None).await.unwrap());

	let pending_tx = middleware.send_transaction(request, None).await.unwrap();

	let result = pending_tx.tx_hash();

	println!("request_tx: {:?}", result);
}

pub async fn user_inbound(
	pub_key: Address,
	priv_key: &str,
	amount: &str,
	metadata: Option<&str>,
	vault_address: &str,
) {
	let parse_vault_address = vault_address
		.parse::<BtcAddress<NetworkUnchecked>>()
		.expect("Invalid BTC address")
		.assume_checked();

	let (bfc_client, btc_provider) = test_set_bfc_client(priv_key).await;

	let registration_pool = bfc_client
		.protocol_contracts
		.registration_pool
		.as_ref()
		.expect("Failed to get registration pool");

	let vault_address = bfc_client
		.contract_call(registration_pool.vault_address(pub_key), "registration_pool.vault_address")
		.await;

	assert_eq!(vault_address, vault_address);

	let transfer_result = send_btc_transaction(
		btc_provider.username.unwrap().as_str(),
		btc_provider.password.unwrap().as_str(),
		btc_provider.provider.as_str().into(),
		parse_vault_address,
		DEFAULT_WALLET_NAME,
		amount,
		metadata,
	)
	.await;
}

pub async fn user_outbound(priv_key: &str, amount: &str, vault_contract_address: &str) {
	let amount_in_satoshis = (amount.parse::<f64>().unwrap() * SAT_DECIMALS) as u64;
	let amount_eth: U256 = U256::from(amount_in_satoshis);

	let (bfc_client, btc_provider) = test_set_bfc_client(priv_key).await;

	let middleware = bfc_client
		.get_provider()
		.clone()
		.wrap_into(|p| SignerMiddleware::new(p, bfc_client.wallet.signer.clone()))
		.wrap_into(|p| NonceManagerMiddleware::new(p, bfc_client.address()));

	let vault_contract = test_get_vault_contract(bfc_client.clone(), vault_contract_address).await;

	let user_request: UserRequest = UserRequest {
		ins_code: Instruction {
			chain: array_bytes::hex2bytes(CHAIN_ID).unwrap().try_into().unwrap(),
			method: array_bytes::hex2bytes(METHOD_ID).unwrap().try_into().unwrap(),
		},
		params: TaskParams {
			token_idx0: array_bytes::hex2bytes(TOKEN_ID_0).unwrap().try_into().unwrap(),
			token_idx1: array_bytes::hex2bytes(TOKEN_ID_1).unwrap().try_into().unwrap(),
			refund: bfc_client.address(),
			to: bfc_client.address(),
			amount: amount_eth,
			variants: Bytes::default(),
		},
	};

	println!("💤 Requesting Outbound (Bifrost -> Bitcoin): {:?}", amount);
	let request = TransactionRequest::new()
		.data(vault_contract.request(user_request).calldata().unwrap())
		.to(vault_contract.address())
		.from(bfc_client.address());

	let request = request
		.clone()
		.gas(middleware.estimate_gas(&TypedTransaction::Legacy(request), None).await.unwrap());

	let nonce = middleware.get_transaction_count(bfc_client.address(), None).await.unwrap();
	println!("nonce: {:?}", nonce);
	let request = request.nonce(nonce);

	let request_tx = middleware.send_transaction(request, None).await;
	match request_tx {
		Ok(pending_tx) => match pending_tx.await {
			Ok(receipt) => {
				println!(
					"⚡️ Request success (Bifrost -> Bitcoin) tx: {:?}",
					receipt.unwrap().transaction_hash
				);
			},
			Err(error) => {
				println!("request failed: {:?}", error);
				return;
			},
		},
		Err(error) => {
			println!("request failed: {:?}", error);
			return;
		},
	}

	// sleep(Duration::from_secs(TIME_SLEEP));

	// let btc_client = test_create_btc_wallet(
	// 	btc_provider.username.unwrap().as_str(),
	// 	btc_provider.password.unwrap().as_str(),
	// 	btc_provider.provider.as_str(),
	// )
	// .await;

	// let finalized_vec = bfc_client
	// 	.contract_call(
	// 		bfc_client.protocol_contracts.socket_queue.as_ref().unwrap().finalized_psbts(),
	// 		"socket_queue.finalized_psbts",
	// 	)
	// 	.await;

	// let psbt =
	// 	Psbt::deserialize(&finalized_vec.first().unwrap()).expect("error on psbt deserialize");

	// let binding = psbt.unsigned_tx.txid();
	// let tx = btc_client.get_raw_transaction(&binding, None).await.unwrap();

	// println!("tx : {:?}", tx);
}
