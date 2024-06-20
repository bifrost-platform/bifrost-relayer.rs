use br_client::btc::block::Event;
use ethers::{
	middleware::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
	providers::Middleware,
	types::{transaction::eip2718::TypedTransaction, TransactionRequest, H160, U256},
	utils::hex,
};
use miniscript::bitcoin::hashes::Hash;
use miniscript::bitcoin::{address::NetworkUnchecked, Address as BtcAddress, Amount, Txid};
use std::{
	// str::FromStr,
	fs::File,
	io::Write,
	str::FromStr,
	thread::sleep,
	time::Duration,
};
mod utils;

use crate::utils::{
	check_registration, create_new_account, get_unified_btc, read_wallet_details, registration,
	send_btc_transaction, test_set_bfc_client, test_set_btc_wallet, transfer_fund, user_inbound,
};

const PUB_KEY: &str = "0xa70e72d66101e4834796115b492b3c650b4b6fb1";
const PRIV_KEY: &str = "0x7d8b5db3afafe575f45841a5d5a1f4bb0ea735416d3b731c089d22d8cd967da2";
const VAULT_ADDRESS: &str = "bcrt1q7nv8cqculhzvgx0mylvqf8wh0epqlylmj436eep6yc4u6d2cemashvehq5";
const AMOUNT: &str = "0.1";
const KEYPAIR_PATH: &str = "../keys";

const WALLET_NAME: &str = "sunouk";
const ALITH_PRIV_KEY: &str = "0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133";
const DEFAULT_WALLET_NAME: &str = "default";

const UNIFIED_BTC_ADDRESS: &str = "0x7554b6e864400b4ec504d0d19c164d33c407b666";
const SAT_DECIMALS: f64 = 100_000_000.0;

#[tokio::test]
async fn test_user_inbound() {
	let amount_in_satoshis = (AMOUNT.parse::<f64>().unwrap() * SAT_DECIMALS) as u64;
	let amount_eth: U256 = U256::from(amount_in_satoshis);
	let parse_vault_address = VAULT_ADDRESS
		.parse::<BtcAddress<NetworkUnchecked>>()
		.expect("Invalid BTC address")
		.assume_checked();

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;

	let unified_btc = get_unified_btc(bfc_client.clone(), UNIFIED_BTC_ADDRESS).await;

	let before_balance = unified_btc.balance_of(bfc_client.address()).await.unwrap();

	println!("before_balance: {}", before_balance);

	user_inbound(PUB_KEY.parse::<H160>().unwrap(), PRIV_KEY, AMOUNT, None, VAULT_ADDRESS).await;

	sleep(Duration::from_secs(30));

	let after_balance = unified_btc.balance_of(bfc_client.address()).await.unwrap();

	println!("after_balance: {}", after_balance);

	let changed_balance = after_balance - before_balance;

	println!("balance: {}", changed_balance);

	assert!(amount_eth > changed_balance && changed_balance > ethers::types::U256::zero());
}

#[tokio::test]
async fn test_transfer_with_metadata() {
	user_inbound(PUB_KEY.parse::<H160>().unwrap(), PRIV_KEY, AMOUNT, Some("test"), VAULT_ADDRESS)
		.await;
}

#[tokio::test]
async fn test_unauthorized_poll_bitcoin_socket() {
	let (bfc_client, _) = test_set_bfc_client(PRIV_KEY).await;

	let middleware = bfc_client
		.get_provider()
		.clone()
		.wrap_into(|p| SignerMiddleware::new(p, bfc_client.wallet.signer.clone()))
		.wrap_into(|p| NonceManagerMiddleware::new(p, bfc_client.address()));

	let bitcoin_socket = bfc_client.protocol_contracts.bitcoin_socket.as_ref().unwrap();

	let event = Event {
		txid: Txid::from_str("0x123").unwrap(),
		index: 0,
		address: BtcAddress::from_str(VAULT_ADDRESS).unwrap(),
		amount: Amount::from_sat(1000),
	};

	let calldata = bitcoin_socket
		.poll(
			event.txid.to_byte_array(),
			event.index.into(),
			bfc_client.address(),
			event.amount.to_sat().into(),
		)
		.calldata()
		.unwrap();

	let request = TransactionRequest::default().data(calldata).to(bitcoin_socket.address());

	let nonce = middleware.get_transaction_count(bfc_client.address(), None).await.unwrap();
	let request = request.nonce(nonce);

	let request = request
		.clone()
		.gas(middleware.estimate_gas(&TypedTransaction::Legacy(request), None).await.unwrap());

	let pending_tx = middleware.send_transaction(request, None).await.unwrap();

	let result = pending_tx.tx_hash();

	println!("request_tx: {:?}", result);
}

#[tokio::test]
async fn test_numerous_inbound_request() {
	let keypair_path = KEYPAIR_PATH.to_string() + "/keypair.json";

	let mut wallet_details_list = read_wallet_details(&keypair_path).await;

	for wallet_details in &mut wallet_details_list {
		let (bfc_client, _) = test_set_bfc_client(&wallet_details.priv_key).await;
		let vault_address =
			check_registration(bfc_client.clone(), &wallet_details.refund_address).await;
		wallet_details.vault_address = vault_address; // Update the vault_address

		user_inbound(
			bfc_client.address(),
			&wallet_details.priv_key,
			AMOUNT,
			None,
			&wallet_details.vault_address,
		)
		.await;
	}

	// Write wallet details to a JSON file
	let json_data = serde_json::to_string_pretty(&wallet_details_list).unwrap();
	let mut file = File::create(keypair_path).unwrap();
	file.write_all(json_data.as_bytes()).unwrap();
}

#[tokio::test]
async fn test_from_multiple_inbound_request() {
	let (wallet, _) = create_new_account().await.unwrap();

	let priv_key = wallet.signer().to_bytes();
	let priv_key_hex = format!("0x{}", hex::encode(priv_key));

	println!("priv_key_hex: {}", priv_key_hex);

	let amount_in_wei = 1_000_000_000_000_000_000u128; // 1 ETH in wei

	let receipt = transfer_fund(&priv_key_hex, amount_in_wei, ALITH_PRIV_KEY).await.unwrap();

	if receipt.is_none() {
		println!("Transaction failed");
	} else {
		println!("Transaction succeess");
	}

	let (bfc_client, btc_provider) = test_set_bfc_client(&priv_key_hex).await;

	let username = btc_provider.username.unwrap();
	let password = btc_provider.password.unwrap();
	let provider = btc_provider.provider;

	let refund_address = match test_set_btc_wallet(
		username.clone().as_str(),
		password.clone().as_str(),
		provider.clone().as_str(),
		WALLET_NAME,
	)
	.await
	{
		Ok(address) => address.trim_matches('"').to_string(),
		Err(e) => {
			println!("Failed to set BTC wallet: {:?}", e);
			return;
		},
	};

	registration(&refund_address, bfc_client.clone()).await;

	sleep(Duration::from_secs(20));

	let vault_address = check_registration(bfc_client, &refund_address).await;

	let parse_vault_address = vault_address
		.parse::<BtcAddress<NetworkUnchecked>>()
		.expect("Invalid BTC address")
		.assume_checked();

	let new_address = match test_set_btc_wallet(
		username.clone().as_str(),
		password.clone().as_str(),
		provider.clone().as_str(),
		WALLET_NAME,
	)
	.await
	{
		Ok(address) => address.trim_matches('"').to_string(),
		Err(e) => {
			println!("Failed to set BTC wallet: {:?}", e);
			return;
		},
	};

	let parse_new_address = new_address
		.parse::<BtcAddress<NetworkUnchecked>>()
		.expect("Invalid BTC address")
		.assume_checked();

	let _ = send_btc_transaction(
		username.clone().as_str(),
		password.clone().as_str(),
		provider.clone().as_str(),
		parse_new_address,
		DEFAULT_WALLET_NAME,
		AMOUNT,
		None,
	)
	.await;

	sleep(Duration::from_secs(20));

	let _ = send_btc_transaction(
		username.clone().as_str(),
		password.clone().as_str(),
		provider.clone().as_str(),
		parse_vault_address.clone(),
		DEFAULT_WALLET_NAME,
		AMOUNT,
		None,
	)
	.await;

	let _ = send_btc_transaction(
		username.clone().as_str(),
		password.clone().as_str(),
		provider.clone().as_str(),
		parse_vault_address.clone(),
		WALLET_NAME,
		AMOUNT,
		None,
	)
	.await;
}
