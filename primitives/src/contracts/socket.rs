use super::*;
use Socket_Struct::*;

pub fn get_asset_oids() -> BTreeMap<&'static str, B256> {
	<BTreeMap<&str, B256>>::from([
		("BFC", b256!("0100010000000000000000000000000000000000000000000000000000000001")),
		("BIFI", b256!("0100010000000000000000000000000000000000000000000000000000000002")),
		("BTC", b256!("0100010000000000000000000000000000000000000000000000000000000003")),
		("ETH", b256!("0100010000000000000000000000000000000000000000000000000000000004")),
		("BNB", b256!("0100010000000000000000000000000000000000000000000000000000000005")),
		("POL", b256!("0100010000000000000000000000000000000000000000000000000000000006")),
		("USDC", b256!("0100010000000000000000000000000000000000000000000000000000000008")),
		("BUSD", b256!("0100010000000000000000000000000000000000000000000000000000000009")),
		("USDT", b256!("010001000000000000000000000000000000000000000000000000000000000a")),
		("DAI", b256!("010001000000000000000000000000000000000000000000000000000000000b")),
		("BTCB", b256!("010001000000000000000000000000000000000000000000000000000000000c")),
		("WBTC", b256!("010001000000000000000000000000000000000000000000000000000000000d")),
		("CBBTC", b256!("010001000000000000000000000000000000000000000000000000000000000e")),
	])
}

sol!(
	#[allow(missing_docs)]
	#[derive(Debug, PartialEq, Eq, Default)]
	#[sol(rpc)]
	SocketContract,
	"../abi/abi.socket.merged.json"
);

sol!(
	#[derive(serde::Serialize, serde::Deserialize)]
	/// Variants structure for socket message parameters.
	/// Follows Solidity ABI encoding: (address sender, address receiver, uint256 max_tx_fee, bytes message)
	struct Variants {
		address sender;
		address receiver;
		uint256 max_tx_fee;
		bytes message;
	}
);

impl From<Signature> for Signatures {
	fn from(signature: Signature) -> Self {
		let r = signature.r().into();
		let s = signature.s().into();
		let v = signature.v() as u8 + 27;

		Signatures { r: vec![r], s: vec![s], v: Bytes::from([v]) }
	}
}

impl From<Vec<Signature>> for Signatures {
	fn from(signatures: Vec<Signature>) -> Self {
		let mut r = Vec::with_capacity(signatures.len());
		let mut s = Vec::with_capacity(signatures.len());
		let mut v = Vec::with_capacity(signatures.len());

		for sig in signatures.iter() {
			r.push(sig.r().into());
			s.push(sig.s().into());
			v.push(sig.v() as u8 + 27);
		}

		Signatures { r, s, v: Bytes::from(v) }
	}
}

impl From<Signatures> for Vec<Signature> {
	fn from(signatures: Signatures) -> Self {
		let mut res = Vec::with_capacity(signatures.r.len());
		for idx in 0..signatures.r.len() {
			let r = signatures.r[idx].into();
			let s = signatures.s[idx].into();
			let v = (signatures.v[idx] - 27) != 0;
			res.push(Signature::new(r, s, v));
		}

		res
	}
}

impl From<RequestID> for FixedBytes<32> {
	fn from(req_id: RequestID) -> Self {
		let mut bytes = [0u8; 32];
		bytes[0..4].copy_from_slice(&req_id.ChainIndex.0);
		bytes[4..12].copy_from_slice(&req_id.round_id.to_be_bytes());
		bytes[12..28].copy_from_slice(&req_id.sequence.to_be_bytes());
		Self::from(bytes)
	}
}

use SocketContract::SocketContractInstance;

pub type SocketInstance<F, P, N> = SocketContractInstance<Arc<FillProvider<F, P, N>>, N>;
