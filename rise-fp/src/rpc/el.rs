use alloy_consensus::Header;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, BlockNumber, Bytes, ChainId, U64};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rlp::Decodable;
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::Block;
use alloy_transport::{RpcError, TransportResult};
use futures::future::try_join_all;

use crate::{
    common::contract::{OptimismPortal, SystemConfig},
    rpc::utils::batch_call,
};

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

pub async fn get_block_headers(
    el_rpc: &RpcClient,
    block_numbers: &[BlockNumber],
) -> TransportResult<Vec<Header>> {
    // 1. Try debug_getRawHeader batch first
    if let Ok(raws) = batch_call(
        el_rpc,
        "debug_getRawHeader",
        block_numbers.iter().map(|bn| (BlockNumberOrTag::Number(*bn),)),
        |resp: Bytes| resp,
    )
    .await
    {
        if let Ok(headers) = raws
            .into_iter()
            .map(|raw| Header::decode(&mut raw.as_ref()))
            .collect::<Result<Vec<_>, _>>()
        {
            return Ok(headers);
        }
    }

    // 2. Fallback: Call get_block_header individually in parallel
    let futures = (block_numbers.iter()) //
        .map(|bn| get_block_header(el_rpc, BlockNumberOrTag::Number(*bn)));
    try_join_all(futures).await
}

pub async fn get_chain_id(el_rpc: &RpcClient) -> TransportResult<ChainId> {
    el_rpc.request_noparams("eth_chainId").map_resp(|resp: U64| resp.to::<u64>()).await
}

pub async fn get_factory_and_registry_addresses(
    l1_rpc: &RpcClient,
    l1_system_config_address: Address,
) -> TransportResult<(Address, Address)> {
    let l1_provider = ProviderBuilder::new().connect_client(l1_rpc.clone());
    let system_config = SystemConfig::new(l1_system_config_address, l1_provider.clone());
    let optimism_portal = match system_config.optimismPortal().call().await {
        Ok(address) => OptimismPortal::new(address, l1_provider.clone()),
        Err(err) => return Err(RpcError::local_usage(err)),
    };
    let (factory, registry) = l1_provider
        .multicall()
        .add(optimism_portal.disputeGameFactory())
        .add(optimism_portal.anchorStateRegistry())
        .aggregate()
        .await
        .map_err(RpcError::local_usage)?;

    Ok((factory, registry))
}
