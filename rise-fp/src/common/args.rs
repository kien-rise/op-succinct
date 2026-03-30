use std::fmt::Debug;

use alloy_network::EthereumWallet;
use alloy_primitives::B256;
use alloy_signer_local::PrivateKeySigner;
use alloy_transport_http::reqwest::Url;
use anyhow::{anyhow, Result};

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

#[derive(Debug, Clone, clap::ValueEnum)]
enum SignerMode {
    Local,
}

pub struct SignerArgs {
    mode: SignerMode,
    private_key: Option<B256>,
}

impl SignerArgs {
    pub fn build_wallet(&self) -> Result<EthereumWallet> {
        match self.mode {
            SignerMode::Local => {
                let pk = self.private_key.ok_or_else(|| anyhow!("private key not provided"))?;
                let signer = PrivateKeySigner::from_bytes(&pk)?;
                return Ok(EthereumWallet::from(signer));
            }
        }
    }
}

// https://github.com/clap-rs/clap/issues/5092
#[derive(Debug, Clone, clap::Args)]
#[group(requires = "proposer.mode")]
pub struct ProposerSignerArgs {
    #[arg(id = "proposer.mode", long = "proposer.mode", required = false)]
    mode: SignerMode,
    #[arg(id = "proposer.private-key", long = "proposer.private-key")]
    private_key: Option<B256>,
}
derive_from!(ProposerSignerArgs, SignerArgs, [mode, private_key]);
derive_from!(&ProposerSignerArgs, SignerArgs, [mode, private_key]);

// https://github.com/clap-rs/clap/issues/5092
#[derive(Debug, Clone, clap::Args)]
#[group(requires = "challenger.mode")]
pub struct ChallengerSignerArgs {
    #[arg(id = "challenger.mode", long = "challenger.mode", required = false)]
    mode: SignerMode,
    #[arg(id = "challenger.private-key", long = "challenger.private-key")]
    private_key: Option<B256>,
}
derive_from!(ChallengerSignerArgs, SignerArgs, [mode, private_key]);
derive_from!(&ChallengerSignerArgs, SignerArgs, [mode, private_key]);
