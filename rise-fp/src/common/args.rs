use std::fmt::Debug;

use alloy_network::EthereumWallet;
use alloy_primitives::B256;
use alloy_signer_local::PrivateKeySigner;
use alloy_transport_http::reqwest::Url;
use anyhow::{bail, Result};

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
#[group(requires = "cl.rpc-url")]
pub struct ClRpcArgs {
    #[arg(long = "cl.rpc-url", id = "cl.rpc-url", required = false)]
    rpc_url: Url,
    #[arg(long = "cl.max-rps", id = "cl.max-rps")]
    max_rps: Option<u32>,
}
derive_from!(ClRpcArgs, RpcArgs, [rpc_url, max_rps]);

pub struct SignerArgs {
    private_key: Option<B256>,
}

impl SignerArgs {
    pub fn build_wallet(&self) -> Result<EthereumWallet> {
        if let Some(pk) = self.private_key {
            let signer = PrivateKeySigner::from_bytes(&pk)?;
            return Ok(EthereumWallet::from(signer));
        }
        bail!("no signer configured")
    }
}

#[derive(Debug, Clone, clap::Args)]
pub struct ProposerSignerArgs {
    #[arg(id = "proposer.private-key", long = "proposer.private-key")]
    private_key: Option<B256>,
}
derive_from!(ProposerSignerArgs, SignerArgs, [private_key]);
derive_from!(&ProposerSignerArgs, SignerArgs, [private_key]);
