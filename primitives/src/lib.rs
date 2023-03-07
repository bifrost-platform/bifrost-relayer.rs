mod socket_event;
pub mod traits;
mod tx_manager;

pub use socket_event::*;
pub use tx_manager::*;

pub type Err = Box<dyn std::error::Error + Send + Sync>;
