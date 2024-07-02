use ethers::types::{H160, U256};
use miniscript::bitcoin::{address::NetworkUnchecked, Address as BtcAddress, Amount};
use std::{collections::HashMap, str::FromStr, thread::sleep, time::Duration};
mod utils;

use crate::utils::{
	build_psbt, get_system_vault, set_btc_client, set_sub_client, submit_roll_back,
	test_set_bfc_client, user_inbound, *,
};

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

	let tx_id = user_inbound(
		PUB_KEY.parse::<H160>().unwrap(),
		PRIV_KEY,
		MAXIMUM_AMOUNT,
		None,
		VAULT_ADDRESS,
	)
	.await
	.unwrap();

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

	let tx_id = user_inbound(
		PUB_KEY.parse::<H160>().unwrap(),
		PRIV_KEY,
		MAXIMUM_AMOUNT,
		None,
		VAULT_ADDRESS,
	)
	.await
	.unwrap();

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
