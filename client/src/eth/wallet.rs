use ethers::{
	prelude::{k256::ecdsa::SigningKey, rand},
	signers::{coins_bip39::English, MnemonicBuilder, Signer},
	types::PathOrString,
};
use std::{fs, path::PathBuf};

use super::ClientErr;

#[derive(Debug)]
pub struct Wallet {
	pub signer: ethers::signers::Wallet<SigningKey>,
}

impl Wallet {
	pub fn new(output_path: PathBuf, chain_id: u32) -> Result<Self, ClientErr> {
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
	) -> Result<Self, ClientErr> {
		let wallet = MnemonicBuilder::<English>::default().phrase(input_path).build()?;

		Ok(Self { signer: wallet.with_chain_id(chain_id) })
	}
}
