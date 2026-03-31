use std::borrow::Cow;

use alloy_json_rpc::{RpcRecv, RpcSend};
use alloy_rpc_client::RpcClient;
use alloy_transport::TransportResult;
use futures::future::try_join_all;

pub async fn batch_call<Params: RpcSend, Resp: RpcRecv, NewOutput>(
    rpc: &RpcClient,
    method: impl Into<Cow<'static, str>>,
    params: impl Iterator<Item = Params>,
    map_resp: impl Fn(Resp) -> NewOutput,
) -> TransportResult<Vec<NewOutput>> {
    let mut batch = rpc.new_batch();
    let mut futures = Vec::new();
    let method = method.into();
    for param in params {
        futures.push(batch.add_call::<Params, Resp>(method.clone(), &param)?.map_resp(&map_resp));
    }
    batch.send().await?;
    try_join_all(futures).await
}
