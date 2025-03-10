use alloy::{
	dyn_abi::DynSolValue,
	primitives::{Address, B256, PrimitiveSignature, U256, keccak256},
};
use k256::{ecdsa::VerifyingKey, elliptic_curve::sec1::ToEncodedPoint};
use sha3::{Digest, Keccak256};

use crate::substrate::{EthereumSignature, Signature};

pub fn sub_display_format(log_target: &str) -> String {
	format!("{:<019}", log_target)
}

/// Encodes the given round and new relayers to bytes.
pub fn encode_roundup_param(round: U256, new_relayers: &[Address]) -> Vec<u8> {
	DynSolValue::Tuple(vec![
		DynSolValue::Uint(round, 256),
		DynSolValue::Array(
			new_relayers.iter().map(|address| DynSolValue::Address(*address)).collect(),
		),
	])
	.abi_encode_params()
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
