use ethers::{
	prelude::{NonceManagerMiddleware, SignerMiddleware},
	providers::Provider,
	signers::LocalWallet,
};
use rand::Rng;
use std::sync::Arc;

mod eip1559_manager;
mod legacy_manager;
mod task;

pub use eip1559_manager::*;
pub use legacy_manager::*;
pub use task::*;

/// The tranaction middleware type used for `TransactionManager`'s.
pub type TransactionMiddleware<T> =
	NonceManagerMiddleware<SignerMiddleware<Arc<Provider<T>>, LocalWallet>>;

/// Generates a random delay that is ranged as 0 to 12000 milliseconds (in milliseconds).
pub fn generate_delay() -> u64 {
	rand::thread_rng().gen_range(0..=12000)
}
