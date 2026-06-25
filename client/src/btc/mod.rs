pub mod block;
pub mod client;
pub mod handlers;
pub mod storage;

pub use client::{Auth, BtcClient};

pub const LOG_TARGET: &str = "bitcoin";
