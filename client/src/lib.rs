pub mod btc;
pub mod eth;

pub type Err = Box<dyn std::error::Error + Send + Sync>;
