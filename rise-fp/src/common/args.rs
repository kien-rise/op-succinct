use std::fmt::Debug;

use alloy_transport_http::reqwest::Url;

use crate::{common::primitives::RpcArgs, derive_from};

#[derive(Debug, clap::Args)]
pub struct L1RpcArgs {
    #[arg(long = "l1.rpc-url", id = "l1.rpc-url")]
    rpc_url: Url,
    #[arg(long = "l1.max-rps", id = "l1.max-rps")]
    max_rps: Option<u32>,
}
derive_from!(L1RpcArgs, RpcArgs, [rpc_url, max_rps]);

#[derive(Debug, clap::Args)]
pub struct L2RpcArgs {
    #[arg(long = "l2.rpc-url", id = "l2.rpc-url")]
    rpc_url: Url,
    #[arg(long = "l2.max-rps", id = "l2.max-rps")]
    max_rps: Option<u32>,
}
derive_from!(L2RpcArgs, RpcArgs, [rpc_url, max_rps]);

#[derive(Debug, clap::Args)]
pub struct ClRpcArgs {
    #[arg(long = "cl.rpc-url", id = "cl.rpc-url")]
    rpc_url: Url,
    #[arg(long = "cl.max-rps", id = "cl.max-rps")]
    max_rps: Option<u32>,
}
derive_from!(ClRpcArgs, RpcArgs, [rpc_url, max_rps]);
