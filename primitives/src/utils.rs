use ethers::{
	types::{Signature as EthersSignature, H256},
	utils::keccak256,
};

use crate::substrate::{EthereumSignature, Signature};

pub fn sub_display_format(log_target: &str) -> String {
	format!("{:<019}", log_target)
}

pub fn convert_ethers_to_ecdsa_signature(ethers_signature: EthersSignature) -> EthereumSignature {
	let sig: String = format!("0x{}", ethers_signature);
	let bytes = sig.as_bytes();

	let mut decode_sig = [0u8; 65];
	decode_sig.copy_from_slice(bytes);

	EthereumSignature(Signature(decode_sig))
}

pub fn hash_bytes(bytes: &Vec<u8>) -> H256 {
	H256::from(keccak256(bytes))
}
