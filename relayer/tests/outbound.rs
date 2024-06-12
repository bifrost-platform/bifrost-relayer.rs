use bitcoincore_rpc::RpcApi;
use br_primitives::contracts::vault::{Instruction, TaskParams, UserRequest};
use ethers::{
	middleware::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
	providers::Middleware,
	types::{transaction::eip2718::TypedTransaction, Bytes, TransactionRequest, H160, U256},
};
use miniscript::bitcoin::Psbt;

use std::thread::sleep;
use tokio::time::Duration;

mod utils;

use crate::utils::{
	get_btc_client, get_btc_wallet_balance, get_unified_btc, read_wallet_details,
	test_get_vault_contract, test_set_bfc_client, user_outbound,
};
const KEYPAIR_PATH: &str = "../keys";
const PRIV_KEY: &str = "0xcbb381dac7c64f449e4b9ad656cd49da86637f542bade20c601821395b5a32c3";
const VAULT_CONTRACT_ADDRESS: &str = "0x6EeE91b7c69e3576C13cE7a9C7C0E305dF6996F9";
const AMOUNT: &str = "0.01";
const WALLET_NAME: &str = "sunouk";
const SAT_DECIMALS: f64 = 100_000_000.0;

#[tokio::test]
async fn test_user_outbound_request() {
	let amount_in_satoshis = (AMOUNT.parse::<f64>().unwrap() * SAT_DECIMALS) as u64;

	let (bfc_client, btc_provider) = test_set_bfc_client(PRIV_KEY).await;

	let before_balance = get_btc_wallet_balance(
		btc_provider.username.clone().unwrap().as_str(),
		btc_provider.password.clone().unwrap().as_str(),
		btc_provider.provider.clone().as_str(),
		WALLET_NAME,
	)
	.await
	.unwrap();

	user_outbound(PRIV_KEY, AMOUNT, VAULT_CONTRACT_ADDRESS).await;

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

	for wallet_details in &mut wallet_details_list {
		user_outbound(&wallet_details.priv_key, AMOUNT, VAULT_CONTRACT_ADDRESS).await;
	}
}
// #[tokio::test]
// async fn test_aunauthorized_submit_finalized_psbt_msg() {
// 	todo!()
// }
