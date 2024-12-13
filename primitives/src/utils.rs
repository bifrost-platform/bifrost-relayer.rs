use alloy::primitives::{keccak256, Address, PrimitiveSignature, B256};
use k256::{ecdsa::VerifyingKey, elliptic_curve::sec1::ToEncodedPoint};
use rand::Rng as _;
use sha3::{Digest, Keccak256};

use crate::substrate::{EthereumSignature, Signature};

pub fn sub_display_format(log_target: &str) -> String {
	format!("{:<019}", log_target)
}

pub fn generate_delay() -> u64 {
	rand::thread_rng().gen_range(0..=12000)
}

impl From<PrimitiveSignature> for EthereumSignature {
	fn from(signature: PrimitiveSignature) -> Self {
		let sig: [u8; 65] = signature.into();
		EthereumSignature(Signature(sig))
	}
}

/// Hash the given bytes.
pub fn hash_bytes(bytes: &Vec<u8>) -> B256 {
	B256::from(keccak256(bytes))
}

/// Recovers the address from the given signature and message.
pub fn recover_message(sig: PrimitiveSignature, msg: &[u8]) -> Address {
	let v = sig.recid();
	let rs = sig.to_k256().unwrap();

	let verify_key =
		VerifyingKey::recover_from_digest(Keccak256::new_with_prefix(msg), &rs, v).unwrap();
	let public_key = k256::PublicKey::from(&verify_key).to_encoded_point(false);
	let hash = keccak256(&public_key.as_bytes()[1..]);

	Address::from_slice(&hash[12..])
}
