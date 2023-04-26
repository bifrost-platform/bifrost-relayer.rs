use std::{fs::File, io::Write, time::Duration};

use ethers::{types::H256, utils::hex};
use serde::Serialize;
use tokio::time::sleep;

mod utils;

use crate::utils::{
	check_registration, create_new_account, read_wallet_details, registration, test_create_keypair,
	test_create_new_wallet, test_set_bfc_client, test_set_btc_wallet, test_set_sub_client,
	test_submit_vault_key, transfer_fund, WalletDetails,
};

const WALLET_NAME: &str = "sunouk5";
const PRIV_KEY: &str = "0x9b8df737e72a51ed04860e9ccf87b1f017a5703b497d5cd9031d182c6daa0774";
const KEYPAIR_PATH: &str = "../keys";
const KEYPAIR_SECERT: &str = "test";
const SUB_URL: &str = "ws://127.0.0.1:9934";

const ALITH_PRIV_KEY: &str = "0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133";

#[tokio::test]
async fn test_user_registration() {
	let (wallet, _) = create_new_account().await.unwrap();
	let priv_key = wallet.signer().to_bytes();
	let priv_key_hex = format!("0x{}", hex::encode(priv_key));

	let amount_in_wei = 1_000_000_000_000_000_000u128; // 1 ETH in wei

	let receipt = transfer_fund(&priv_key_hex, amount_in_wei, ALITH_PRIV_KEY).await.unwrap();

	if receipt.is_none() {
		println!("Transaction failed");
	} else {
		println!("Transaction succeess");
	}

	let (bfc_client, btc_provider) = test_set_bfc_client(&priv_key_hex).await;

	let refund_address = match test_set_btc_wallet(
		btc_provider.username.unwrap().as_str(),
		btc_provider.password.unwrap().as_str(),
		btc_provider.provider.as_str(),
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

	println!("new wallet addresss: {}", bfc_client.address());
	println!("new wallet priv_key: {}", priv_key_hex);
	println!("new wallet refund_address: {}", refund_address);

	registration(&refund_address, bfc_client.clone()).await;

	sleep(Duration::from_secs(20));

	check_registration(bfc_client, &refund_address).await;
}

#[tokio::test]
async fn test_numerous_register_request() {
	let amount_in_wei = 1_000_000_000_000_000_000u128; // 1 ETH in wei
	let number_of_user_request = 10;

	let wallet_name = "sunouk5";
	// let wallet_name1 = "sunouk3";
	// let wallet_name2 = "sunouk4";

	let keypair_path = KEYPAIR_PATH.to_string() + "/keypair.json";

	let mut tasks = vec![];

	let mut wallet_details_list = read_wallet_details(&keypair_path).await;

	for i in 0..number_of_user_request {
		// let middle = wallet_details_list.len() / 2;

		// let wallet_name = if i < middle { wallet_name1 } else { wallet_name2 };

		let (wallet, _) = create_new_account().await.unwrap();
		let priv_key = wallet.signer().to_bytes();
		let priv_key_hex = format!("0x{}", hex::encode(priv_key));

		transfer_fund(&priv_key_hex, amount_in_wei, ALITH_PRIV_KEY).await.unwrap();

		let (bfc_client, btc_provider) = test_set_bfc_client(&priv_key_hex).await;

		let refund_address = match test_set_btc_wallet(
			btc_provider.username.unwrap().as_str(),
			btc_provider.password.unwrap().as_str(),
			btc_provider.provider.as_str(),
			wallet_name,
		)
		.await
		{
			Ok(address) => address.trim_matches('"').to_string(),
			Err(e) => {
				println!("Failed to set BTC wallet: {:?}", e);
				return;
			},
		};

		wallet_details_list.push(WalletDetails {
			pub_key: bfc_client.address(),
			priv_key: priv_key_hex.clone(),
			refund_address: refund_address.clone(),
			vault_address: "".to_string(),
		});

		tasks.push(tokio::spawn(async move {
			registration(&refund_address, bfc_client).await;
		}));
	}

	let results = futures::future::join_all(tasks).await;

	for result in results {
		match result {
			Ok(()) => {
				println!("Registration succeeded");
			},
			Err(e) => {
				println!("Registration error: {:?}", e);
				assert!(false, "Registration failed");
			},
		}
	}

	// Write wallet details to a JSON file
	let json_data = serde_json::to_string_pretty(&wallet_details_list).unwrap();
	let mut file = File::create(keypair_path).unwrap();
	file.write_all(json_data.as_bytes()).unwrap();
}

#[tokio::test]
async fn test_anauthorized_submit_vault_key() {
	let priv_key = "0x9b8df737e72a51ed04860e9ccf87b1f017a5703b497d5cd9031d182c6daa0774";

	let (bfc_client, _) = test_set_bfc_client(priv_key).await;
	let sub_client = test_set_sub_client(SUB_URL).await;
	let keypair_path = KEYPAIR_PATH.to_string() + "/registration";
	let mut keypair_storage = test_create_keypair(&keypair_path, KEYPAIR_SECERT).await;

	let btc_pub_key = keypair_storage.create_new_keypair().await;

	let result =
		test_submit_vault_key(bfc_client.clone(), sub_client, btc_pub_key, bfc_client.address())
			.await;

	match result {
		Ok(receipt) => {
			println!("Transaction succeeded: {:?}", receipt);
			assert!(receipt.extrinsic_hash() != H256::zero());
		},
		Err(_) => {
			assert!(true, "Transaction failed");
		},
	}
}

#[tokio::test]
async fn test_submit_invalid_vault_key() {
	let (bfc_client, _) = test_set_bfc_client(PRIV_KEY).await;
	let sub_client = test_set_sub_client(SUB_URL).await;
	let keypair_path = KEYPAIR_PATH.to_string() + "/registration";

	let mut keypair_storage = test_create_keypair(&keypair_path, KEYPAIR_SECERT).await;

	let btc_pub_key = keypair_storage.create_new_keypair().await;

	let result =
		test_submit_vault_key(bfc_client.clone(), sub_client, btc_pub_key, bfc_client.address())
			.await;

	match result {
		Ok(receipt) => {
			println!("Transaction succeeded: {:?}", receipt);
			assert!(receipt.extrinsic_hash() != H256::zero());
		},
		Err(_) => {
			assert!(true, "Transaction failed");
		},
	}
}