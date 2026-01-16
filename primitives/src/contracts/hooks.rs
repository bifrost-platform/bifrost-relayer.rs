use crate::contracts;

use super::*;

sol!(
	#[allow(missing_docs)]
	#[derive(Debug)]
	#[sol(rpc)]
	HooksContract,
	"../abi/abi.hooks.json"
);

use HooksContract::HooksContractInstance;

impl From<contracts::socket::Socket_Struct::Socket_Message>
	for contracts::hooks::Socket_Struct::Socket_Message
{
	fn from(msg: contracts::socket::Socket_Struct::Socket_Message) -> Self {
		Self {
			req_id: contracts::hooks::Socket_Struct::RequestID {
				chain: msg.req_id.ChainIndex,
				round_id: msg.req_id.round_id,
				sequence: msg.req_id.sequence,
			},
			status: msg.status,
			ins_code: contracts::hooks::Socket_Struct::Instruction {
				chain: msg.ins_code.ChainIndex,
				method: msg.ins_code.RBCmethod,
			},
			params: contracts::hooks::Socket_Struct::Task_Params {
				tokenIDX0: msg.params.tokenIDX0,
				tokenIDX1: msg.params.tokenIDX1,
				refund: msg.params.refund,
				to: msg.params.to,
				amount: msg.params.amount,
				variants: msg.params.variants,
			},
		}
	}
}

pub type HooksInstance<F, P, N> = HooksContractInstance<Arc<FillProvider<F, P, N>>, N>;
