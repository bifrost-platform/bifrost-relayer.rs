use ethers::{types::H256, utils::keccak256};

pub fn sub_display_format(log_target: &str) -> String {
	format!("{:<019}", log_target)
}

/// Hash the given bytes.
pub fn hash_bytes(bytes: &Vec<u8>) -> H256 {
	H256::from(keccak256(bytes))
}
