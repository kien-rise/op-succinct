use std::{path::PathBuf, sync::Arc};

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{BlockHash, BlockNumber};
use alloy_rpc_client::RpcClient;
use alloy_transport_http::reqwest::Url;
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use rise_fp::{
    common::{
        args::{ClRpcArgs, L1RpcArgs, L2RpcArgs},
        primitives::RpcArgs,
    },
    proof::host::{build_partial_witness, create_backend, RiseCfg},
    rpc::{
        cl::{self, SafeDBClient},
        el,
    },
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    PrecacheWitness(PrecacheWitnessCmd),
}

#[derive(clap::Args)]
struct PrecacheWitnessCmd {
    #[arg(long)]
    claim_block: BlockNumber,
    #[arg(long)]
    start_block: BlockNumber,
    #[command(flatten)]
    l1_rpc_args: L1RpcArgs,
    #[command(flatten)]
    l2_rpc_args: L2RpcArgs,
    #[command(flatten)]
    cl_rpc_args: ClRpcArgs,
    #[arg(long)]
    l1_beacon_address: String,
    #[arg(long)]
    eigenda_proxy_address: Url,
    #[arg(long)]
    output: Option<PathBuf>,
}

impl PrecacheWitnessCmd {
    async fn run(self) -> Result<()> {
        tracing::info!(?self.start_block, ?self.claim_block, "Prove");

        let l1_rpc: RpcClient = RpcArgs::from(self.l1_rpc_args).into();
        let l2_rpc: RpcClient = RpcArgs::from(self.l2_rpc_args).into();
        let cl_rpc: RpcClient = RpcArgs::from(self.cl_rpc_args).into();

        let l1_head: BlockHash = {
            match SafeDBClient::new(cl_rpc.clone(), 64)
                .l2_to_l1_safe(self.claim_block, BlockNumberOrTag::Finalized)
                .await?
            {
                Some(num_hash) => num_hash.hash,
                None => bail!("cannot find l1_safe"),
            }
        };

        let (agreed_l2_output_root, agreed_l2_head_hash) = {
            let resp = cl::get_output_at_block(&cl_rpc, self.start_block).await?;
            assert!(resp.block_ref.block_info.number == self.start_block);
            (resp.output_root, resp.block_ref.block_info.hash)
        };

        let (claimed_l2_output_root, claimed_l2_block_number) = {
            let resp = cl::get_output_at_block(&cl_rpc, self.claim_block).await?;
            assert!(resp.block_ref.block_info.number == self.claim_block);
            (resp.output_root, resp.block_ref.block_info.number)
        };

        let l2_chain_id = el::get_chain_id(&l2_rpc).await?;

        let rollup_config = Arc::new(cl::get_rollup_config(&cl_rpc).await?);

        let l1_config = match kona_registry::L1_CONFIGS.get(&rollup_config.l1_chain_id) {
            Some(l1_config) => Arc::new(l1_config.clone()),
            None => bail!("cannot find l1_config"),
        };

        let cfg = RiseCfg {
            l1_head,
            agreed_l2_head_hash,
            agreed_l2_output_root,
            claimed_l2_output_root,
            claimed_l2_block_number,
            l2_chain_id,
            rollup_config,
            l1_config,
        };

        let backend = create_backend(
            cfg,
            l1_rpc.clone(),
            l2_rpc.clone(),
            self.l1_beacon_address,
            self.eigenda_proxy_address,
        )
        .await;

        let partial_witness = build_partial_witness(backend, l1_rpc).await?;

        if let Some(path) = self.output {
            if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e != "cbor") {
                tracing::warn!(?path, "Writing CBOR using a file extension other than .cbor");
            }
            let mut buf = Vec::new();
            ciborium::into_writer(&partial_witness, &mut buf)?;
            tokio::fs::write(&path, buf).await?;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(match std::env::var(EnvFilter::DEFAULT_ENV) {
            Ok(var) => EnvFilter::builder().parse(var)?,
            Err(_) => EnvFilter::new("info"),
        })
        .init();

    match args.command {
        Commands::PrecacheWitness(precache_witness_cmd) => {
            precache_witness_cmd.run().await?;
        }
    }
    Ok(())
}
