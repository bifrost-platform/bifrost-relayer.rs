use alloy::primitives::{Address, address};

/// The address representing the native currency of the chain.
/// This is a reserved address used to identify native assets (e.g. ETH, BNB, BFC) in the bridge.
pub const NATIVE_CURRENCY_ADDRESS: Address = address!("ffffffffffffffffffffffffffffffffffffffff");
