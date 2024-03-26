async fn test_with_client() {
	let cli = Cli::from_args();

	let tokio_runtime = build_runtime().unwrap();
	let configuration =
		create_configuration(tokio_runtime.handle().clone(), cli.load_spec()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_submit_vault_key_succeeds() {}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn submit_unsigned_psbt() {}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn submit_signed_psbt() {}
