use bitcoincore_rpc::RpcApi;
use br_primitives::{
	contracts::vault::{Instruction, TaskParams, UserRequest},
	substrate::{bifrost_runtime, AccountId20, SignedPsbtMessage},
	utils::convert_ethers_to_ecdsa_signature,
};
use ethers::{
	middleware::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
	providers::Middleware,
	types::{transaction::eip2718::TypedTransaction, Bytes, TransactionRequest, H160, U256},
};
use miniscript::bitcoin::{address::NetworkUnchecked, Address as BtcAddress, Amount, Txid};
use miniscript::bitcoin::{Psbt, TxOut};

use std::{str::FromStr, thread::sleep};
use tokio::time::Duration;

mod utils;

use crate::utils::{
	get_btc_wallet_balance, get_unified_btc, read_wallet_details, set_btc_client, set_sub_client,
	test_create_keypair, test_get_vault_contract, test_set_bfc_client, transfer_fund,
	user_outbound,
};
const KEYPAIR_PATH: &str = "../keys";
const KEYPAIR_SECERT: &str = "test";

const PRIV_KEY: &str = "0x74f37e4466d643b54fb7432420c88944b0e004b3f2e181c15caf4eb5e407da58";
const VAULT_CONTRACT_ADDRESS: &str = "0x9f24feAf7E3c193511537371CBA969dA25B26752";
const AMOUNT: &str = "0.01";
const WALLET_NAME: &str = "sunouk";
const SAT_DECIMALS: f64 = 100_000_000.0;

const ALITH_PRIV_KEY: &str = "0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133";

const SUB_URL: &str = "ws://127.0.0.1:9934";

const WALLET_NAME_PREFIX: &str = "BRP-";

#[tokio::test]
async fn test_user_outbound_request() {
	let amount_btc: Amount = Amount::from_str("0.01 BTC").unwrap();
	let amount_in_satoshis = amount_btc.to_sat();

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;

	let before_balance = get_btc_wallet_balance(
		btc_provider.username.clone().unwrap().as_str(),
		btc_provider.password.clone().unwrap().as_str(),
		btc_provider.provider.clone().as_str(),
		WALLET_NAME,
	)
	.await
	.unwrap();

	user_outbound(PRIV_KEY, AMOUNT, VAULT_CONTRACT_ADDRESS, bfc_client.address()).await;

	sleep(Duration::from_secs(20));

	let after_balance = get_btc_wallet_balance(
		btc_provider.username.clone().unwrap().as_str(),
		btc_provider.password.clone().unwrap().as_str(),
		btc_provider.provider.clone().as_str(),
		WALLET_NAME,
	)
	.await
	.unwrap();

	println!("before: {}", before_balance);
	println!("after: {}", after_balance);

	let changed_balance = (after_balance - before_balance).to_sat();

	println!("balance: {}", changed_balance);

	assert!(amount_in_satoshis > changed_balance && changed_balance > 0);
}

#[tokio::test]
async fn test_numerous_outbound_request() {
	let keypair_path = KEYPAIR_PATH.to_string() + "/keypair.json";

	let mut wallet_details_list = read_wallet_details(&keypair_path).await;

	let amount_in_wei = 1_000_000_000_000_000_000u128; // 1 ETH in wei

	for wallet_details in &mut wallet_details_list {
		let receipt = transfer_fund(&wallet_details.priv_key, amount_in_wei, ALITH_PRIV_KEY)
			.await
			.unwrap();

		let (bfc_client, btc_provider) =
			test_set_bfc_client(&wallet_details.priv_key.clone()).await;

		user_outbound(
			&wallet_details.priv_key,
			AMOUNT,
			VAULT_CONTRACT_ADDRESS,
			bfc_client.address(),
		)
		.await;
	}
}

#[tokio::test]
async fn test_from_multiple_outbound_request() {
	let keypair_path = KEYPAIR_PATH.to_string() + "/keypair.json";

	let mut wallet_details_list = read_wallet_details(&keypair_path).await;

	let amount_in_wei = 1_000_000_000_000_000_000u128; // 1 ETH in wei

	let (bfc_client, btc_provider) =
		test_set_bfc_client("0x4a1a45af850c37293da1a8804ab8b20e103db087d6699e90add1b7a64a4a70cb")
			.await;

	for wallet_details in &mut wallet_details_list {
		// let receipt = transfer_fund(&wallet_details.priv_key, amount_in_wei, ALITH_PRIV_KEY)
		// 	.await
		// 	.unwrap();

		user_outbound(
			&wallet_details.priv_key,
			AMOUNT,
			VAULT_CONTRACT_ADDRESS,
			bfc_client.address(),
		)
		.await;
	}
}

#[tokio::test]
async fn test_replay_submit() {
	let amount_btc: Amount = Amount::from_str("0.01 BTC").unwrap();
	let amount_in_satoshis = amount_btc.to_sat();
	let amount_in_wei = 1_000_000_000_000_000_000u128;

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;
	let sub_client = set_sub_client(SUB_URL).await;
	let btc_client =
		set_btc_client(btc_provider.clone(), Some(WALLET_NAME_PREFIX), Some(sub_client.clone()))
			.await;

	let before_balance = get_btc_wallet_balance(
		btc_provider.username.clone().unwrap().as_str(),
		btc_provider.password.clone().unwrap().as_str(),
		btc_provider.provider.clone().as_str(),
		WALLET_NAME,
	)
	.await
	.unwrap();

	let receipt = transfer_fund(&PRIV_KEY, amount_in_wei, ALITH_PRIV_KEY).await.unwrap();

	user_outbound(PRIV_KEY, AMOUNT, VAULT_CONTRACT_ADDRESS, bfc_client.address()).await;

	sleep(Duration::from_secs(20));

	let finalized_vec = loop {
		let vec = bfc_client
			.contract_call(
				bfc_client.protocol_contracts.socket_queue.as_ref().unwrap().finalized_psbts(),
				"socket_queue.finalized_psbts",
			)
			.await;

		if vec.len() > 0 {
			break vec;
		} else {
			sleep(Duration::from_secs(1));
		}
	};

	println!("finalized_vec: {:?}", finalized_vec);

	let psbt =
		Psbt::deserialize(&finalized_vec.first().unwrap()).expect("error on psbt deserialize");

	let tx_id = btc_client
		.send_raw_transaction(&psbt.clone().extract_tx().expect("fee rate too high"))
		.await
		.unwrap();

	println!("tx_id: {}", tx_id);
}

#[tokio::test]
async fn test_manipulate_submit() {
	let amount_btc: Amount = Amount::from_str("0.01 BTC").unwrap();
	let amount_in_satoshis = amount_btc.to_sat();
	let amount_in_wei = 1_000_000_000_000_000_000u128;

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;
	let sub_client = set_sub_client(SUB_URL).await;
	let btc_client =
		set_btc_client(btc_provider.clone(), Some(WALLET_NAME_PREFIX), Some(sub_client.clone()))
			.await;
	let keypair_path = KEYPAIR_PATH.to_string() + "/registration";
	let mut keypair_storage = test_create_keypair(&keypair_path, KEYPAIR_SECERT).await;

	let receipt = transfer_fund(&PRIV_KEY, amount_in_wei, ALITH_PRIV_KEY).await.unwrap();

	user_outbound(PRIV_KEY, AMOUNT, VAULT_CONTRACT_ADDRESS, bfc_client.address()).await;

	let unsigned_vec = loop {
		let vec = bfc_client
			.contract_call(
				bfc_client.protocol_contracts.socket_queue.as_ref().unwrap().unsigned_psbts(),
				"socket_queue.unsigned_psbts",
			)
			.await;

		if vec.len() > 0 {
			break vec;
		} else {
			sleep(Duration::from_secs(1));
		}
	};

	let unsigned_psbt = unsigned_vec.first().unwrap();

	let origin_unsigned_psbt = Psbt::deserialize(&unsigned_psbt).unwrap();
	let mut psbt = Psbt::deserialize(&unsigned_psbt).unwrap();

	let manipulate_address = BtcAddress::from_str("bcrt1qdu8356nygd0fk44rq75q7qvj7fneesq52mell6")
		.unwrap()
		.assume_checked();

	let manipulate_tx_out =
		TxOut { value: amount_btc, script_pubkey: manipulate_address.script_pubkey() };

	psbt.unsigned_tx.output.push(manipulate_tx_out);

	let (msg, signature) = if keypair_storage.sign_psbt(&mut psbt) {
		let signed_psbt = psbt.serialize();
		let msg = SignedPsbtMessage {
			authority_id: AccountId20(bfc_client.address().0),
			unsigned_psbt: origin_unsigned_psbt.serialize(),
			signed_psbt: signed_psbt.clone(),
		};
		let signature =
			convert_ethers_to_ecdsa_signature(bfc_client.wallet.sign_message(signed_psbt.as_ref()));
		(msg, signature)
	} else {
		panic!("error on sign psbt");
	};

	let payload = bifrost_runtime::tx().btc_socket_queue().submit_signed_psbt(msg, signature);

	let events = sub_client
		.tx()
		.create_unsigned(&payload)
		.unwrap()
		.submit_and_watch()
		.await
		.unwrap()
		.wait_for_finalized_success()
		.await
		.unwrap();

	println!("events: {:?}", events);
}
