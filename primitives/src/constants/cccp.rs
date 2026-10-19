use alloy::primitives::{Address, address};

/// The address representing the native currency of the chain.
/// This is a reserved address used to identify native assets (e.g. ETH, BNB, BFC) in the bridge.
pub const NATIVE_CURRENCY_ADDRESS: Address = address!("ffffffffffffffffffffffffffffffffffffffff");

/// The maximum byte size of a single socket message on runtimes without `BtcSocketQueue`
/// (e.g. the private mainnet hub), matching that runtime's 2 KiB default.
pub const DEFAULT_MAX_SOCKET_MESSAGE_BYTES: u32 = 2 * 1024;
