use crate::citrea_rpc::CitreaRpcClient;
use crate::error::Result;

const BATCH_RPC_SIZE: usize = 100;

/// Count total L2 transactions in a block range using batched JSON-RPC calls.
/// Each batch covers `l2_start_block..=l2_end_block` (typically 1000 blocks).
pub fn count_l2_txs_in_range(
    rpc: &CitreaRpcClient,
    l2_start_block: u64,
    l2_end_block: u64,
) -> Result<u64> {
    let block_numbers: Vec<u64> = (l2_start_block..=l2_end_block).collect();
    let results = rpc.get_block_tx_counts_batch(&block_numbers, BATCH_RPC_SIZE)?;
    let total: u64 = results.iter().map(|&(_, count)| count).sum();
    Ok(total)
}

/// Aggregated gas data for a batch of L2 blocks.
pub struct GasAggregates {
    pub total_used: u64,
    pub total_limit: u64,
    pub min_used: u64,
    pub max_used: u64,
}

/// Aggregate gas usage across an L2 block range using batched JSON-RPC calls.
pub fn aggregate_gas_in_range(
    rpc: &CitreaRpcClient,
    l2_start_block: u64,
    l2_end_block: u64,
) -> Result<GasAggregates> {
    let block_numbers: Vec<u64> = (l2_start_block..=l2_end_block).collect();
    let results = rpc.get_block_gas_batch(&block_numbers, BATCH_RPC_SIZE)?;

    let mut total_used: u64 = 0;
    let mut total_limit: u64 = 0;
    let mut min_used: u64 = u64::MAX;
    let mut max_used: u64 = 0;

    for &(_block_num, gas_used, gas_limit) in &results {
        total_used += gas_used;
        total_limit += gas_limit;
        if gas_used < min_used {
            min_used = gas_used;
        }
        if gas_used > max_used {
            max_used = gas_used;
        }
    }

    // Handle empty range
    if results.is_empty() {
        min_used = 0;
    }

    Ok(GasAggregates {
        total_used,
        total_limit,
        min_used,
        max_used,
    })
}
