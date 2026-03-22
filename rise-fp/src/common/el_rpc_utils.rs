use alloy_consensus::Header;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::Bytes;
use alloy_rlp::Decodable;
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::Block;
use alloy_transport::{RpcError, TransportResult};

pub async fn get_block_header(
    el_rpc: &RpcClient,
    block_number: BlockNumberOrTag,
) -> TransportResult<Header> {
    // 1. Try debug_getRawHeader first
    if let Ok(raw) = el_rpc.request::<_, Bytes>("debug_getRawHeader", (block_number,)).await {
        if let Ok(header) = Header::decode(&mut raw.as_ref()) {
            return Ok(header)
        }
    }

    // 2. Fallback to standard eth_getBlockByNumber
    match el_rpc.request::<_, Option<Block>>("eth_getBlockByNumber", (block_number, false)).await? {
        Some(block) => Ok(block.into_consensus_header()),
        None => Err(RpcError::NullResp),
    }
}
