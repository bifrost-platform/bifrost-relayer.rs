use ethers::{
	prelude::{k256::ecdsa::SigningKey, rand},
	signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer},
	types::{Address, PathOrString, Signature, U256},
	utils::{hex::FromHex, keccak256},
};
use k256::{
	ecdsa::{SigningKey as K256SigningKey, VerifyingKey},
	elliptic_curve::sec1::ToEncodedPoint,
	PublicKey as K256PublicKey,
};
use sha3::{Digest, Keccak256};
use std::{fs, path::PathBuf};

type WalletResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug)]
pub struct WalletManager {
	pub signer: ethers::signers::Wallet<SigningKey>,
	secret_key: Option<K256SigningKey>,
}

impl WalletManager {
	pub fn new(output_path: PathBuf, chain_id: u32) -> WalletResult<Self> {
		let mut rng = rand::thread_rng();

		fs::create_dir_all(&output_path)?;

		let wallet = MnemonicBuilder::<English>::default()
			.write_to(output_path)
			.build_random(&mut rng)?;

		Ok(Self { signer: wallet.with_chain_id(chain_id), secret_key: None })
	}

	pub fn from_phrase_or_file<P: Into<PathOrString>>(
		input_path: P,
		chain_id: u32,
	) -> WalletResult<Self> {
		let wallet = MnemonicBuilder::<English>::default().phrase(input_path).build()?;

		Ok(Self { signer: wallet.with_chain_id(chain_id), secret_key: None })
	}

	pub fn from_private_key(private_key: &str, chain_id: u32) -> WalletResult<Self> {
		let wallet = private_key.parse::<LocalWallet>().expect("private key should exist");

		let pk_bytes =
			<[u8; 32]>::from_hex(private_key.to_string().trim_start_matches("0x")).unwrap();
		let signing_key = K256SigningKey::from_bytes(&pk_bytes.into()).unwrap();

		Ok(Self { signer: wallet.with_chain_id(chain_id), secret_key: Some(signing_key) })
	}

	pub fn sign_message(&self, msg: &[u8]) -> Signature {
		let digest = Keccak256::new_with_prefix(msg);
		let (sig, recovery_id) =
			self.secret_key.clone().unwrap().sign_digest_recoverable(digest).unwrap();

		let (r, s) = sig.split_bytes();

		Signature {
			r: U256::from_big_endian(r.as_slice()),
			s: U256::from_big_endian(s.as_slice()),
			v: (recovery_id.to_byte() + 27).into(),
		}
	}

	/// Recovers the given signature and returns the signer address.
	pub fn recover_message(&self, sig: Signature, msg: &[u8]) -> Address {
		let r: [u8; 32] = sig.r.into();
		let s: [u8; 32] = sig.s.into();
		let v = sig.recovery_id().unwrap();

		let rs = k256::ecdsa::Signature::from_slice([r, s].concat().as_slice()).unwrap();

		let verify_key =
			VerifyingKey::recover_from_digest(Keccak256::new_with_prefix(msg), &rs, v).unwrap();

		let public_key = K256PublicKey::from(&verify_key).to_encoded_point(false);
		let hash = keccak256(&public_key.as_bytes()[1..]);

		Address::from_slice(&hash[12..])
	}

	pub fn address(&self) -> Address {
		self.signer.address()
	}
}
