use std::{borrow::Cow, collections::BTreeMap, ops::Range};

use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use alloy_json_rpc::{RpcRecv, RpcSend};
use alloy_primitives::{BlockNumber, B256};
use alloy_rpc_client::RpcClient;
use alloy_transport::TransportResult;
use futures::future::try_join_all;
use kona_protocol::{L2BlockInfo, SyncStatus};
use serde::{Deserialize, Serialize};

/// An [output response][or] for Optimism Rollup.
///
/// [or]: https://github.com/ethereum-optimism/optimism/blob/f20b92d3eb379355c876502c4f28e72a91ab902f/op-service/eth/output.go#L10-L17
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputResponse {
    /// The output version.
    pub version: B256,
    /// The output root hash.
    pub output_root: B256,
    /// A reference to the L2 block.
    pub block_ref: L2BlockInfo,
    /// The withdrawal storage root.
    pub withdrawal_storage_root: B256,
    /// The state root.
    pub state_root: B256,
    /// The status of the node sync.
    pub sync_status: SyncStatus,
}

/// The safe head response.
///
/// <https://github.com/ethereum-optimism/optimism/blob/77c91d09eaa44d2c53bec60eb89c5c55737bc325/op-service/eth/output.go#L19-L22>
/// Note: the optimism "eth.BlockID" type is number,hash <https://github.com/ethereum-optimism/optimism/blob/77c91d09eaa44d2c53bec60eb89c5c55737bc325/op-service/eth/id.go#L10-L13>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeHeadResponse {
    /// The L1 block.
    pub l1_block: BlockNumHash,
    /// The safe head.
    pub safe_head: BlockNumHash,
}

#[derive(Debug)]
pub struct SafeDBClient {
    cl_rpc: RpcClient,
    batch_size: usize,
    cache: BTreeMap<BlockNumber, (BlockNumber, BlockNumber)>,
}

impl SafeDBClient {
    pub fn new(cl_rpc: RpcClient, batch_size: usize) -> Self {
        Self { cl_rpc, batch_size, cache: BTreeMap::new() }
    }

    pub async fn l1_to_l2_safe(
        &mut self,
        l1_block_number: BlockNumber,
        hint_l2_block_number: Option<BlockNumber>,
    ) -> TransportResult<BlockNumber> {
        if let Some(bn) = hint_l2_block_number {
            if let Some((&l2_safe, &(l1_first, l1_last))) = self.cache.range(bn..).next() {
                if l1_first <= l1_block_number && l1_block_number <= l1_last {
                    return Ok(l2_safe);
                }
            }
        }

        let resp = self
            .cl_rpc
            .request::<_, SafeHeadResponse>(
                "optimism_safeHeadAtL1Block",
                &(BlockNumberOrTag::Number(l1_block_number),),
            )
            .await?;
        Ok(resp.safe_head.number)
    }

    async fn query_l1_and_cache(
        &mut self,
        l1_block_numbers: Vec<BlockNumber>,
    ) -> TransportResult<()> {
        let mut batch = self.cl_rpc.new_batch();
        let mut futures = Vec::new();

        for bn in l1_block_numbers.iter() {
            futures.push(
                batch
                    .add_call::<_, SafeHeadResponse>(
                        "optimism_safeHeadAtL1Block",
                        &(BlockNumberOrTag::Number(*bn),),
                    )?
                    .map_resp(|resp| (resp.safe_head.number, (resp.l1_block.number, *bn))),
            );
        }
        batch.send().await?;

        let answers = try_join_all(futures).await?;
        for (l2_safe, (l1_first, l1_last)) in answers {
            use std::collections::btree_map::Entry;
            match self.cache.entry(l2_safe) {
                Entry::Vacant(vac) => {
                    vac.insert((l1_first, l1_last));
                }
                Entry::Occupied(mut occ) => {
                    let (first, last) = *occ.get();
                    occ.insert((Ord::min(first, l1_first), Ord::max(last, l1_last)));
                }
            }
        }
        Ok(())
    }

    pub async fn l2_to_l1_safe(
        &mut self,
        l2_block_number: BlockNumber,
        l1_block_range: Range<BlockNumber>,
    ) -> TransportResult<BlockNumber> {
        let (mut start, mut end) = (l1_block_range.start, l1_block_range.end);

        loop {
            // 1. Use the cache to shorten (start, end)
            if let Some((l1_first, _l1_last)) = self.cache.get(&l2_block_number) {
                return Ok(*l1_first);
            }
            if let Some((_, (_, l1_last))) = self.cache.range(..l2_block_number).next_back() {
                start = Ord::max(start, *l1_last + 1);
            }
            if let Some((_, (l1_first, _))) = self.cache.range(l2_block_number..).next() {
                end = Ord::min(end, *l1_first)
            }
            if start == end {
                return Ok(start);
            }

            // 2. Perform some queries to enrich the cache
            let bs = self.batch_size as u64;
            let l1_block_numbers: Vec<_> = if end - start <= bs {
                (start..end).collect()
            } else {
                let skips = bresenham(end - start - bs, bs + 1);
                let mut current = start;
                let mut queries = Vec::with_capacity(self.batch_size);
                for i in 0..self.batch_size {
                    current += skips[i];
                    queries.push(current);
                    current += 1;
                }
                queries
            };
            self.query_l1_and_cache(l1_block_numbers).await?;
        }
    }

    pub async fn l2_to_l1_origin(
        &self,
        l2_block_number: BlockNumber,
    ) -> TransportResult<BlockNumHash> {
        self.cl_rpc
            .request::<_, OutputResponse>(
                "optimism_outputAtBlock",
                (BlockNumberOrTag::Number(l2_block_number),),
            )
            .map_resp(|resp| resp.block_ref.l1_origin)
            .await
    }
}

/// Distributes `sum` into a vector of `len` integers such that each element
/// differs by at most 1, ensuring the most uniform distribution possible.
///
/// ```
/// sum = x[0] + x[1] + ... + x[len-1]
/// ```
fn bresenham(sum: u64, len: u64) -> Vec<u64> {
    debug_assert!(len > 0);
    let mut terms = Vec::with_capacity(len as usize);
    let base = sum / len;
    let mut acc = 0;
    for _ in 0..len {
        acc += sum % len;
        if acc >= len {
            acc -= len;
            terms.push(base + 1);
        } else {
            terms.push(base);
        }
    }
    terms
}

pub async fn get_output_at_block(
    cl_rpc: &RpcClient,
    l2_block_number: BlockNumber,
) -> TransportResult<OutputResponse> {
    cl_rpc
        .request::<_, OutputResponse>(
            "optimism_outputAtBlock",
            (BlockNumberOrTag::Number(l2_block_number),),
        )
        .await
}

pub async fn batch_call<Params: RpcSend, Resp: RpcRecv, NewOutput>(
    rpc: &RpcClient,
    method: impl Into<Cow<'static, str>>,
    params: impl Iterator<Item = Params>,
    map_resp: impl Fn(Resp) -> NewOutput,
) -> TransportResult<Vec<NewOutput>>
{
    let mut batch = rpc.new_batch();
    let mut futures = Vec::new();
    let method = method.into();
    for param in params {
        futures.push(batch.add_call::<Params, Resp>(method.clone(), &param)?.map_resp(&map_resp));
    }
    batch.send().await?;
    try_join_all(futures).await
}
