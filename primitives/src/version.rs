use alloy::primitives::{B256, U256};

/// The multiplier applied to the major version. (`major * 1_000_000`)
const MAJOR_MULTIPLIER: u64 = 1_000_000;
/// The multiplier applied to the minor version. (`minor * 1_000`)
const MINOR_MULTIPLIER: u64 = 1_000;
/// The exclusive upper bound of the minor and patch versions.
const MINOR_PATCH_LIMIT: u64 = 1_000;

/// Encodes a semantic version (e.g. `3.0.2`) to a number. (`major * 1_000_000 + minor * 1_000 +
/// patch`)
///
/// Pre-release and build metadata (e.g. `-rc.1`, `+abc`) are ignored.
/// Returns `None` if the version is malformed, or if the minor/patch exceeds `999`.
pub fn encode_impl_version(version: &str) -> Option<U256> {
	let core = version.split(['-', '+']).next()?;

	let mut parts = core.split('.');
	let major = parts.next()?.parse::<u64>().ok()?;
	let minor = parts.next()?.parse::<u64>().ok()?;
	let patch = parts.next()?.parse::<u64>().ok()?;
	if parts.next().is_some() || minor >= MINOR_PATCH_LIMIT || patch >= MINOR_PATCH_LIMIT {
		return None;
	}

	major
		.checked_mul(MAJOR_MULTIPLIER)?
		.checked_add(minor * MINOR_MULTIPLIER)?
		.checked_add(patch)
		.map(U256::from)
}

/// Encodes a git commit hash (hex) to `bytes32`. The hash is placed at the front, and the
/// remaining bytes are right-padded with zeros. (e.g. `cde7b950f42` -> `0xcde7b950f420000…`)
///
/// Returns zero bytes if the hash is unavailable (e.g. `unknown` when built without git, such as
/// inside docker), or is not a valid hex string.
pub fn encode_spec_version(commit_hash: &str) -> B256 {
	let hash = commit_hash.trim();
	if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
		return B256::ZERO;
	}
	// pad on the hex string so an odd number of nibbles (e.g. an 11 digit short hash) is handled.
	format!("{hash:0<64}").parse::<B256>().unwrap_or(B256::ZERO)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn impl_version_is_encoded_from_semver() {
		assert_eq!(encode_impl_version("3.0.2"), Some(U256::from(3_000_002u64)));
		assert_eq!(encode_impl_version("3.0.3"), Some(U256::from(3_000_003u64)));
		assert_eq!(encode_impl_version("12.34.567"), Some(U256::from(12_034_567u64)));
	}

	#[test]
	fn impl_version_ignores_pre_release_and_build_metadata() {
		assert_eq!(encode_impl_version("3.0.2-rc.1"), Some(U256::from(3_000_002u64)));
		assert_eq!(encode_impl_version("3.0.2+build.5"), Some(U256::from(3_000_002u64)));
	}

	#[test]
	fn impl_version_rejects_malformed_or_overflowing_versions() {
		assert_eq!(encode_impl_version(""), None);
		assert_eq!(encode_impl_version("3.0"), None);
		assert_eq!(encode_impl_version("3.0.2.1"), None);
		assert_eq!(encode_impl_version("a.b.c"), None);
		assert_eq!(encode_impl_version("3.1000.0"), None);
		assert_eq!(encode_impl_version("3.0.1000"), None);
	}

	#[test]
	fn spec_version_is_right_padded_with_zeros() {
		let encoded = encode_spec_version("cde7b950f42");
		assert_eq!(&encoded[..6], &[0xcd, 0xe7, 0xb9, 0x50, 0xf4, 0x20]);
		assert!(encoded[6..].iter().all(|b| *b == 0));

		let even = encode_spec_version("cde7b950f4");
		assert_eq!(&even[..5], &[0xcd, 0xe7, 0xb9, 0x50, 0xf4]);
		assert!(even[5..].iter().all(|b| *b == 0));
	}

	#[test]
	fn spec_version_falls_back_to_zero_bytes_when_hash_is_unavailable() {
		assert_eq!(encode_spec_version("unknown"), B256::ZERO);
		assert_eq!(encode_spec_version(""), B256::ZERO);
		assert_eq!(encode_spec_version("0xcde7b950f42"), B256::ZERO);
		assert_eq!(encode_spec_version(&"a".repeat(65)), B256::ZERO);
	}
}
