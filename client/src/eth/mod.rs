mod events;
pub use events::*;

use web3::{
	ethabi::Address,
	types::{Block, BlockId, BlockNumber, H256, U256, U64},
	Transport, Web3,
};

#[derive(Debug, Clone)]
pub struct EthClient<T: Transport> {
	web3: Web3<T>,
}

impl<T: Transport> EthClient<T> {
	pub fn new(url: &str) -> EthClient<web3::transports::Http> {
		match web3::transports::Http::new(url) {
			Ok(transport) => {
				let web3 = Web3::new(transport);
				EthClient { web3 }
			},
			Err(error) => panic!("Error on initiating EthClient: {:?}", error),
		}
	}

	pub async fn get_balance(
		&self,
		address: Address,
		block: Option<BlockNumber>,
	) -> web3::Result<U256> {
		Ok(self.web3.eth().balance(address, block).await?)
	}

	pub async fn get_block_number(&self) -> web3::Result<U64> {
		Ok(self.web3.eth().block_number().await?)
	}

	pub async fn get_block_by_id(&self, id: BlockId) -> web3::Result<Option<Block<H256>>> {
		Ok(self.web3.eth().block(id).await?)
	}
}
