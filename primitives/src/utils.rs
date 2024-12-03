use alloy::primitives::{keccak256, Address, Signature as EthersSignature, B256};
use k256::{ecdsa::VerifyingKey, elliptic_curve::sec1::ToEncodedPoint};
use sha3::{Digest, Keccak256};

use crate::substrate::{EthereumSignature, Signature};

pub fn sub_display_format(log_target: &str) -> String {
	format!("{:<019}", log_target)
}

/// Converts the ethers::Signature to a bifrost_runtime::Signature.
pub fn convert_ethers_to_ecdsa_signature(ethers_signature: EthersSignature) -> EthereumSignature {
	let sig: [u8; 65] = ethers_signature.into();
	EthereumSignature(Signature(sig))
}

/// Hash the given bytes.
pub fn hash_bytes(bytes: &Vec<u8>) -> B256 {
	B256::from(keccak256(bytes))
}

/// Recovers the address from the given signature and message.
pub fn recover_message(sig: EthersSignature, msg: &[u8]) -> Address {
	let r = sig.r().to_be_bytes::<32>();
	let s = sig.s().to_be_bytes::<32>();
	let v = sig.v().recid();
	let rs = k256::ecdsa::Signature::from_slice([r, s].concat().as_slice()).unwrap();

	let verify_key =
		VerifyingKey::recover_from_digest(Keccak256::new_with_prefix(msg), &rs, v).unwrap();
	let public_key = k256::PublicKey::from(&verify_key).to_encoded_point(false);
	let hash = keccak256(&public_key.as_bytes()[1..]);

	Address::from_slice(&hash[12..])
}
