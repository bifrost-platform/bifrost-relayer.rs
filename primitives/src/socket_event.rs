use crate::Err;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub struct SocketEvent {
	pub rid: RequestId,
	pub request_status: usize,
	pub instruction: Instruction,
	pub params: Params,
}

impl SocketEvent {
	pub fn from(s: &str) -> Result<SocketEvent, Err> {
		serde_json::from_str(s).map_err(Into::into)
	}
}

#[derive(Deserialize, Serialize, Debug)]
pub struct RequestId {
	pub requested_chain: usize,
	pub round: usize,
	pub sequence_number: usize,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Instruction {
	pub dst_chain: usize,
	#[serde(deserialize_with = "de_string_to_bytes")]
	pub instruction: Vec<u8>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Params {
	#[serde(deserialize_with = "de_string_to_bytes")]
	pub asset1: Vec<u8>,
	#[serde(deserialize_with = "de_string_to_bytes")]
	pub asset2: Vec<u8>,
	#[serde(deserialize_with = "de_string_to_bytes")]
	pub sender: Vec<u8>,
	#[serde(deserialize_with = "de_string_to_bytes")]
	pub receiver: Vec<u8>,
	pub amount: usize,
	#[serde(deserialize_with = "de_string_to_bytes")]
	pub variants: Vec<u8>,
}

pub fn de_string_to_bytes<'de, D>(de: D) -> Result<Vec<u8>, D::Error>
where
	D: Deserializer<'de>,
{
	let s: &str = Deserialize::deserialize(de)?;
	Ok(s.as_bytes().to_vec())
}
