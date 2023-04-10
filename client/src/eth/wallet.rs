use ethers::{
	prelude::{k256::ecdsa::SigningKey, rand},
	signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer},
	types::{PathOrString, H160},
};
use std::{fs, path::PathBuf};

use super::TxResult;

#[derive(Debug)]
pub struct WalletManager {
	pub signer: ethers::signers::Wallet<SigningKey>,
}

impl WalletManager {
	pub fn new(output_path: PathBuf, chain_id: u32) -> TxResult<Self> {
		let mut rng = rand::thread_rng();

		fs::create_dir_all(&output_path)?;

		let wallet = MnemonicBuilder::<English>::default()
			.write_to(output_path)
			.build_random(&mut rng)?;

		Ok(Self { signer: wallet.with_chain_id(chain_id) })
	}

	pub fn from_phrase_or_file<P: Into<PathOrString>>(
		input_path: P,
		chain_id: u32,
	) -> TxResult<Self> {
		let wallet = MnemonicBuilder::<English>::default().phrase(input_path).build()?;

		Ok(Self { signer: wallet.with_chain_id(chain_id) })
	}

	pub fn from_private_key(input_path: &str, chain_id: u32) -> TxResult<Self> {
		let wallet = input_path.parse::<LocalWallet>()?;

		Ok(Self { signer: wallet.with_chain_id(chain_id) })
	}

	pub fn address(&self) -> H160 {
		self.signer.address()
	}
}
