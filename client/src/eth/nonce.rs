use alloy::{
	network::Network,
	primitives::Address,
	providers::{Provider, fillers::NonceManager},
	transports::TransportResult,
};
use std::{
	collections::HashMap,
	sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::Mutex;

/// Sentinel indicating that the nonce must be (re)fetched from the chain.
const UNSET: u64 = u64::MAX;

/// A cached [`NonceManager`] that can be re-synced with the chain.
///
/// Behaves like alloy's `CachedNonceManager`: the first nonce of an account is fetched from the
/// chain (`pending`), then incremented locally so that concurrent sends never pick the same nonce.
/// Unlike `CachedNonceManager`, the cache can be invalidated with [`Self::reset`]. A nonce is
/// consumed as soon as it is handed out, so a send that never lands on-chain (rejected,
/// dropped from the mempool, reorged out) leaves the cache ahead of the chain and every later
/// transaction stuck behind the gap. Resetting makes the next send start from the chain's nonce
/// again, which fixes both a cache that is ahead and one that is behind (`nonce too low`).
#[derive(Clone, Debug, Default)]
pub struct RelayerNonceManager {
	nonces: Arc<StdMutex<HashMap<Address, Arc<Mutex<u64>>>>>,
}

impl RelayerNonceManager {
	fn entry(&self, address: Address) -> Arc<Mutex<u64>> {
		self.nonces
			.lock()
			.unwrap()
			.entry(address)
			.or_insert_with(|| Arc::new(Mutex::new(UNSET)))
			.clone()
	}

	/// Invalidate the cached nonce of every account. The next transaction of each refetches it
	/// from the chain.
	///
	/// All accounts are reset rather than only the default one, since transactions may be sent
	/// from other signers too (e.g. a roundup relayed as the previous round's relayer).
	pub async fn reset(&self) {
		let entries: Vec<_> = self.nonces.lock().unwrap().values().cloned().collect();
		for entry in entries {
			*entry.lock().await = UNSET;
		}
	}
}

#[async_trait::async_trait]
impl NonceManager for RelayerNonceManager {
	async fn get_next_nonce<P, N>(&self, provider: &P, address: Address) -> TransportResult<u64>
	where
		P: Provider<N>,
		N: Network,
	{
		let entry = self.entry(address);
		let mut nonce = entry.lock().await;
		let next = if *nonce == UNSET {
			provider.get_transaction_count(address).pending().await?
		} else {
			*nonce + 1
		};
		*nonce = next;
		Ok(next)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloy::{
		primitives::{U64, address},
		providers::ProviderBuilder,
		transports::mock::Asserter,
	};

	const ACCOUNT: Address = address!("0x32b7fcbf9a680d510efe4e514d9d18bb1bef2fbf");

	#[tokio::test]
	async fn increments_locally_and_resyncs_after_reset() {
		let asserter = Asserter::new();
		let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());
		let manager = RelayerNonceManager::default();

		// first nonce comes from the chain, then increments locally without RPC calls
		asserter.push_success(&U64::from(2282));
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 2282);
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 2283);
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 2284);

		// cache ahead of the chain (2283, 2284 never landed): reset moves it back
		manager.reset().await;
		asserter.push_success(&U64::from(2283));
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 2283);

		// cache behind the chain (`nonce too low`): reset moves it forward
		manager.reset().await;
		asserter.push_success(&U64::from(2290));
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 2290);
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 2291);
	}

	#[tokio::test]
	async fn reset_covers_every_signer() {
		const OTHER: Address = address!("0x00000000000000000000000000000000000000aa");
		let asserter = Asserter::new();
		let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());
		let manager = RelayerNonceManager::default();

		asserter.push_success(&U64::from(10));
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 10);
		asserter.push_success(&U64::from(50));
		assert_eq!(manager.get_next_nonce(&provider, OTHER).await.unwrap(), 50);

		manager.reset().await;
		asserter.push_success(&U64::from(10));
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 10);
		asserter.push_success(&U64::from(49));
		assert_eq!(manager.get_next_nonce(&provider, OTHER).await.unwrap(), 49);
	}

	#[tokio::test]
	async fn failed_fetch_keeps_cache_unset() {
		let asserter = Asserter::new();
		let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());
		let manager = RelayerNonceManager::default();

		asserter.push_failure_msg("connection reset");
		assert!(manager.get_next_nonce(&provider, ACCOUNT).await.is_err());

		asserter.push_success(&U64::from(7));
		assert_eq!(manager.get_next_nonce(&provider, ACCOUNT).await.unwrap(), 7);
	}
}
