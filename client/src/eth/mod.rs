use br_primitives::{
	constants::{
		config::{BOOTSTRAP_BLOCK_OFFSET, NATIVE_BLOCK_TIME},
		errors::{INSUFFICIENT_FUNDS, INVALID_CHAIN_ID, PROVIDER_INTERNAL_ERROR},
	},
	contracts::authority::BfcStaking::round_meta_data,
	eth::{AggregatorContracts, ProtocolContracts, ProviderMetadata},
	tx::TxRequestMetadata,
	utils::generate_delay,
};

use alloy::{
	network::Ethereum,
	primitives::{
		utils::{format_units, parse_ether, Unit},
		Address, ChainId,
	},
	providers::{
		fillers::{FillProvider, TxFiller},
		PendingTransactionBuilder, Provider, RootProvider, SendableTx, WalletProvider,
	},
	rpc::types::TransactionRequest,
	signers::{local::LocalSigner, Signature, Signer as _},
	transports::{Transport, TransportResult},
};
use eyre::{eyre, Result};
use k256::ecdsa::SigningKey;
use sc_service::SpawnTaskHandle;
use std::{sync::Arc, time::Duration};
use url::Url;

pub mod events;
pub mod handlers;
pub mod traits;

#[derive(Clone)]
pub struct EthClient<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// The inner provider.
	inner: Arc<FillProvider<F, P, T, Ethereum>>,
	/// The signer.
	pub signer: LocalSigner<SigningKey>,
	/// The provider metadata.
	pub metadata: ProviderMetadata,
	/// The protocol contracts.
	pub protocol_contracts: ProtocolContracts<F, P, T>,
	/// The aggregator contracts.
	pub aggregator_contracts: AggregatorContracts<F, P, T>,
}

impl<F, P, T> EthClient<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	/// Create a new EthClient
	pub fn new(
		inner: Arc<FillProvider<F, P, T, Ethereum>>,
		signer: LocalSigner<SigningKey>,
		metadata: ProviderMetadata,
		protocol_contracts: ProtocolContracts<F, P, T>,
		aggregator_contracts: AggregatorContracts<F, P, T>,
	) -> Self {
		Self { inner, signer, metadata, protocol_contracts, aggregator_contracts }
	}

	/// Verifies whether the configured chain id and the provider's chain id match.
	pub async fn verify_chain_id(&self) -> Result<()> {
		let chain_id = self.get_chain_id().await?;
		if chain_id != self.metadata.id {
			Err(eyre!(INVALID_CHAIN_ID))
		} else {
			Ok(())
		}
	}

	/// Verifies whether the relayer has enough balance to pay for the transaction fees.
	pub async fn verify_minimum_balance(&self) -> Result<()> {
		if self.metadata.is_native {
			let balance = self.get_balance(self.address()).await?;
			if balance < parse_ether("1")? {
				eyre::bail!(INSUFFICIENT_FUNDS)
			}
		}
		Ok(())
	}

	/// Get the signer address.
	pub fn address(&self) -> Address {
		self.inner.default_signer_address()
	}

	/// Get the chain name.
	pub fn get_chain_name(&self) -> String {
		self.metadata.name.clone()
	}

	/// Returns the URL of the provider.
	pub fn get_url(&self) -> Url {
		self.metadata.url.clone()
	}
	/// Sync the native token balance to the metrics.
	pub async fn sync_balance(&self) -> Result<()> {
		br_metrics::set_native_balance(
			&self.get_chain_name(),
			format_units(self.get_balance(self.address()).await?, "ether")?.parse::<f64>()?,
		);
		Ok(())
	}

	/// Get the chain id.
	pub fn chain_id(&self) -> ChainId {
		self.metadata.id
	}

	/// Get the bitcoin chain id.
	pub fn get_bitcoin_chain_id(&self) -> Option<ChainId> {
		self.metadata.bitcoin_chain_id
	}

	/// Signs the given message.
	pub async fn sign_message(&self, message: &[u8]) -> Result<Signature> {
		let a: Vec<u8> = self.signer.sign_message(message).await?.into();
		Ok(Signature::try_from(a.as_slice())?)
	}

	/// Get the bootstrap offset height based on the block time.
	/// Approximately Bifrost: 3s, Polygon: 2s, BSC: 3s, Ethereum: 12s
	pub async fn get_bootstrap_offset_height_based_on_block_time(
		&self,
		round_offset: u64,
		round_info: round_meta_data,
	) -> Result<u64> {
		let block_number = self.get_block_number().await?;
		let prev_block_number = block_number.saturating_sub(BOOTSTRAP_BLOCK_OFFSET);
		let block_diff = block_number.checked_sub(prev_block_number).unwrap();

		let current_block = self.get_block(block_number.into(), true.into()).await?;
		let prev_block = self.get_block(prev_block_number.into(), true.into()).await?;
		if let (Some(current_block), Some(prev_block)) = (current_block, prev_block) {
			let current_timestamp = current_block.header.timestamp;
			let prev_timestamp = prev_block.header.timestamp;
			let timestamp_diff = current_timestamp.checked_sub(prev_timestamp).unwrap() as f64;
			let avg_block_time = timestamp_diff / block_diff as f64;

			let blocks = round_offset
				.checked_mul(round_info.round_length.saturating_to::<u64>())
				.unwrap();
			let blocks_to_native_chain_time = blocks.checked_mul(NATIVE_BLOCK_TIME).unwrap();
			let bootstrap_offset_height = blocks_to_native_chain_time as f64 / avg_block_time;
			Ok(bootstrap_offset_height.ceil() as u64)
		} else {
			Err(eyre!(PROVIDER_INTERNAL_ERROR))
		}
	}

	/// Verifies whether the current relayer was selected at the current round
	pub async fn is_selected_relayer(&self) -> Result<bool> {
		let relayer_manager = self.protocol_contracts.relayer_manager.as_ref().unwrap();
		Ok(relayer_manager.is_selected_relayer(self.address(), false).call().await?._0)
	}
}

#[async_trait::async_trait]
impl<F, P, T> Provider<T, Ethereum> for EthClient<F, P, T>
where
	F: TxFiller + WalletProvider,
	P: Provider<T>,
	T: Transport + Clone,
{
	fn root(&self) -> &RootProvider<T, Ethereum> {
		self.inner.root()
	}

	async fn send_transaction_internal(
		&self,
		tx: SendableTx<Ethereum>,
	) -> TransportResult<PendingTransactionBuilder<T, Ethereum>> {
		self.inner.send_transaction_internal(tx).await
	}
}

pub fn send_transaction<F, P, T>(
	client: Arc<EthClient<F, P, T>>,
	mut request: TransactionRequest,
	requester: String,
	metadata: TxRequestMetadata,
	handle: SpawnTaskHandle,
) where
	F: TxFiller + WalletProvider + 'static,
	P: Provider<T> + 'static,
	T: Transport + Clone,
{
	handle.spawn("send_transaction", None, async move {
		if client.metadata.is_native {
			// gas price is fixed to 1000 Gwei on bifrost network
			request.max_fee_per_gas = Some(0);
			request.max_priority_fee_per_gas = Some(1000 * Unit::GWEI.wei().to::<u128>());
		} else {
			// to avoid duplicate(will revert) external networks transactions
			tokio::time::sleep(Duration::from_millis(generate_delay())).await;

			if !client.metadata.eip1559 {
				// TODO: if transaction failed by gas price issue, should bring back `GasCoefficient` here.
				request.gas_price = Some(client.get_gas_price().await.unwrap());
			}
		}

		match client.send_transaction(request).await {
			Ok(pending) => {
				log::info!(
					target: &requester,
					" 🔖 Send transaction ({} tx:{}): {}",
					client.get_chain_name(),
					pending.tx_hash(),
					metadata
				);
			},
			Err(err) => {
				let msg = format!(
					" ❗️ Failed to send transaction ({} address:{}): {}, Error: {}",
					client.get_chain_name(),
					client.address(),
					metadata,
					err
				);
				log::error!(target: &requester, "{msg}");
				sentry::capture_message(&msg, sentry::Level::Error);
			},
		}
	});
}
