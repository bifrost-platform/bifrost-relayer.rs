use ethers::{
	types::{Signature as EthersSignature, H256},
	utils::keccak256,
};

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
pub fn hash_bytes(bytes: &Vec<u8>) -> H256 {
	H256::from(keccak256(bytes))
}
