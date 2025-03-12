use super::*;
use Socket_Struct::*;

pub fn get_asset_oids() -> BTreeMap<&'static str, B256> {
	<BTreeMap<&str, B256>>::from([
		("BFC", b256!("0100010000000000000000000000000000000000000000000000000000000001")),
		("BIFI", b256!("0100010000000000000000000000000000000000000000000000000000000002")),
		("BTC", b256!("0100010000000000000000000000000000000000000000000000000000000003")),
		("ETH", b256!("0100010000000000000000000000000000000000000000000000000000000004")),
		("BNB", b256!("0100010000000000000000000000000000000000000000000000000000000005")),
		("MATIC", b256!("0100010000000000000000000000000000000000000000000000000000000006")),
		("AVAX", b256!("0100010000000000000000000000000000000000000000000000000000000007")),
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

impl From<PrimitiveSignature> for Signatures {
	fn from(signature: PrimitiveSignature) -> Self {
		let r = signature.r().into();
		let s = signature.s().into();
		let v = signature.v() as u8 + 27;

		Signatures { r: vec![r], s: vec![s], v: Bytes::from([v]) }
	}
}

impl From<Vec<PrimitiveSignature>> for Signatures {
	fn from(signatures: Vec<PrimitiveSignature>) -> Self {
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

impl From<Signatures> for Vec<PrimitiveSignature> {
	fn from(signatures: Signatures) -> Self {
		let mut res = Vec::with_capacity(signatures.r.len());
		for idx in 0..signatures.r.len() {
			let r = signatures.r[idx].into();
			let s = signatures.s[idx].into();
			let v = (signatures.v[idx] - 27) != 0;
			res.push(PrimitiveSignature::new(r, s, v));
		}

		res
	}
}

use SocketContract::SocketContractInstance;

pub type SocketInstance<F, P> =
	SocketContractInstance<(), Arc<FillProvider<F, P, AnyNetwork>>, AnyNetwork>;
