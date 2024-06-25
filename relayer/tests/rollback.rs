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
	collections::HashMap,
	fs::File,
	io::Write,
	str::FromStr,
	thread::sleep,
	time::Duration,
};
mod utils;

use crate::utils::{
	build_psbt, check_registration, create_new_account, get_system_vault, get_unified_btc,
	read_wallet_details, registration, send_btc_transaction, set_btc_client, set_sub_client,
	submit_roll_back, test_set_bfc_client, test_set_btc_wallet, transfer_fund, user_inbound,
};

const PUB_KEY: &str = "0xf43f0e6cd614a4b3ac1aab60862569e80186bd0a";
const PRIV_KEY: &str = "0x18a4893b8d51fced698fb065e592b41e5350b7fbbdf62fcc63f52f77a64ef73c";
const REFUND_ADDRESS: &str = "bcrt1qujsq6pdrxt0nznv657ltjtrk97932l7axgj7mk";
const VAULT_ADDRESS: &str = "bcrt1qk9fuwsdwawf45ll052cnfww7u8v6wpc80cr3993juh9h3ahy690qdttt34";

const AMOUNT: &str = "0.1";
const KEYPAIR_PATH: &str = "../keys";

const WALLET_NAME: &str = "sunouk";
const DEFAULT_WALLET_NAME: &str = "default";

const UNIFIED_BTC_ADDRESS: &str = "0x7554b6e864400b4ec504d0d19c164d33c407b666";
const SAT_DECIMALS: f64 = 100_000_000.0;

const MINIMUM_AMOUNT: &str = "0.000235";
const MAXIMUM_AMOUNT: &str = "5.1";

const SUB_URL: &str = "ws://127.0.0.1:9934";

const WALLET_NAME_PREFIX: &str = "BRP-";

#[tokio::test]
async fn test_minimum_rollback() {
	let amount_btc = Amount::from_str("0.000235 BTC").unwrap();
	let amount_in_satoshis = amount_btc.to_sat();
	let amount_eth: U256 = U256::from(amount_in_satoshis);

	// let amount_btc = Amount::from_sat(amount_in_satoshis);
	let parse_refund_address = REFUND_ADDRESS
		.parse::<BtcAddress<NetworkUnchecked>>()
		.expect("Invalid BTC address");

	let tx_id = user_inbound(
		PUB_KEY.parse::<H160>().unwrap(),
		PRIV_KEY,
		MINIMUM_AMOUNT,
		None,
		VAULT_ADDRESS,
	)
	.await
	.unwrap();

	sleep(Duration::from_secs(20));

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;

	let sub_client = set_sub_client(SUB_URL).await;
	let btc_client =
		set_btc_client(btc_provider, Some(WALLET_NAME_PREFIX), Some(sub_client.clone())).await;

	let system_vault = get_system_vault(bfc_client.clone()).await;

	let mut request = HashMap::new();

	request.insert(parse_refund_address, amount_btc);

	let psbt_bytes = build_psbt(request, &system_vault, btc_client.clone()).await;

	let event = submit_roll_back(
		bfc_client.clone(),
		btc_client.clone(),
		sub_client,
		tx_id.as_str().unwrap(),
		psbt_bytes,
		bfc_client.address(),
		VAULT_ADDRESS,
		amount_eth,
	)
	.await;

	println!("event: {:?}", event);
}

#[tokio::test]
async fn test_maximum_rollback() {
	let amount_btc: Amount = Amount::from_str("5.1 BTC").unwrap();
	let amount_in_satoshis = amount_btc.to_sat();
	let amount_eth: U256 = U256::from(amount_in_satoshis);
	let parse_refund_address = REFUND_ADDRESS
		.parse::<BtcAddress<NetworkUnchecked>>()
		.expect("Invalid BTC address");

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;

	let sub_client = set_sub_client(SUB_URL).await;
	let btc_client =
		set_btc_client(btc_provider, Some(WALLET_NAME_PREFIX), Some(sub_client.clone())).await;

	let unified_btc = get_unified_btc(bfc_client.clone(), UNIFIED_BTC_ADDRESS).await;

	let before_balance = unified_btc.balance_of(bfc_client.address()).await.unwrap();

	let tx_id = user_inbound(
		PUB_KEY.parse::<H160>().unwrap(),
		PRIV_KEY,
		MAXIMUM_AMOUNT,
		None,
		VAULT_ADDRESS,
	)
	.await
	.unwrap();

	sleep(Duration::from_secs(20));

	let system_vault = get_system_vault(bfc_client.clone()).await;

	let mut request = HashMap::new();

	request.insert(parse_refund_address, amount_btc);

	let psbt_bytes = build_psbt(request, &system_vault, btc_client.clone()).await;

	let event = submit_roll_back(
		bfc_client.clone(),
		btc_client.clone(),
		sub_client,
		tx_id.as_str().unwrap(),
		psbt_bytes,
		bfc_client.address(),
		VAULT_ADDRESS,
		amount_eth,
	)
	.await;

	println!("event: {:?}", event);
}

#[tokio::test]
async fn test_replay_rollback_submit() {
	let amount_btc: Amount = Amount::from_str("5.1 BTC").unwrap();
	let amount_in_satoshis = amount_btc.to_sat();
	let amount_eth: U256 = U256::from(amount_in_satoshis);
	let parse_refund_address = REFUND_ADDRESS
		.parse::<BtcAddress<NetworkUnchecked>>()
		.expect("Invalid BTC address");

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;

	let sub_client = set_sub_client(SUB_URL).await;
	let btc_client =
		set_btc_client(btc_provider, Some(WALLET_NAME_PREFIX), Some(sub_client.clone())).await;

	let unified_btc = get_unified_btc(bfc_client.clone(), UNIFIED_BTC_ADDRESS).await;

	let before_balance = unified_btc.balance_of(bfc_client.address()).await.unwrap();

	let tx_id = user_inbound(
		PUB_KEY.parse::<H160>().unwrap(),
		PRIV_KEY,
		MAXIMUM_AMOUNT,
		None,
		VAULT_ADDRESS,
	)
	.await
	.unwrap();

	sleep(Duration::from_secs(20));

	let system_vault = get_system_vault(bfc_client.clone()).await;

	let mut request = HashMap::new();

	request.insert(parse_refund_address, amount_btc);

	let psbt_bytes = build_psbt(request, &system_vault, btc_client.clone()).await;

	let event = submit_roll_back(
		bfc_client.clone(),
		btc_client.clone(),
		sub_client.clone(),
		tx_id.as_str().unwrap(),
		psbt_bytes.clone(),
		bfc_client.address(),
		VAULT_ADDRESS,
		amount_eth,
	)
	.await;

	println!("event1: {:?}", event);

	let event = submit_roll_back(
		bfc_client.clone(),
		btc_client.clone(),
		sub_client,
		tx_id.as_str().unwrap(),
		psbt_bytes,
		bfc_client.address(),
		VAULT_ADDRESS,
		amount_eth,
	)
	.await;

	println!("event2: {:?}", event);
}
