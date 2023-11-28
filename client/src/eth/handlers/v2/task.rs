use br_primitives::eth::ChainID;

const SUB_LOG_TARGET: &str = "filter-task";

pub struct ExecutionFilterTask {
	chain_id: ChainID,
}
