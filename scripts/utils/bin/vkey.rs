use alloy_primitives::B256;
use alloy_rpc_client::RpcClient;
use alloy_transport_http::reqwest::Url;
use anyhow::Context;
use clap::{Parser, Subcommand};
use kona_genesis::RollupConfig;
use op_succinct_client_utils::boot::hash_rollup_config;
use op_succinct_elfs::AGGREGATION_ELF;
use op_succinct_proof_utils::get_range_elf_embedded;
use sp1_sdk::{HashableKey, Prover, ProverClient};

use std::path::PathBuf;

#[derive(Debug, Clone, Parser)]
#[command(name = "vkey")]
#[command(about = "OP Succinct verification key utilities")]
pub struct VkeyArgs {
    /// The environment file to use.
    #[arg(long)]
    pub env_file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: VkeyCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum VkeyCommand {
    /// Get the range verification key hash
    RangeVkey { elf_path: Option<PathBuf> },
    /// Get the aggregation verification key hash
    AggregationVkey { elf_path: Option<PathBuf> },
    /// Get the rollup config hash
    RollupConfigHash {
        #[arg(env = "L2_NODE_RPC")]
        l2_node_rpc: Url,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = VkeyArgs::parse();

    if let Some(path) = &args.env_file {
        dotenv::from_path(path).context("Failed to load env file")?;
    }

    match args.command {
        VkeyCommand::RangeVkey { elf_path } => {
            let prover = ProverClient::builder().cpu().build();
            let (_, range_vk) = if let Some(path) = &elf_path {
                let elf_bytes = std::fs::read(path)
                    .with_context(|| format!("Failed to read ELF: {}", path.display()))?;
                prover.setup(&elf_bytes)
            } else {
                prover.setup(get_range_elf_embedded())
            };
            println!("{}", B256::from(range_vk.hash_bytes()));
        }
        VkeyCommand::AggregationVkey { elf_path } => {
            let prover = ProverClient::builder().cpu().build();
            let (_, agg_vk) = if let Some(path) = &elf_path {
                prover.setup(
                    &std::fs::read(path)
                        .with_context(|| format!("Failed to read ELF: {}", path.display()))?,
                )
            } else {
                prover.setup(AGGREGATION_ELF)
            };
            println!("{}", B256::from(agg_vk.bytes32_raw()));
        }
        VkeyCommand::RollupConfigHash { l2_node_rpc } => {
            let rpc_client = RpcClient::new_http(l2_node_rpc);
            let rollup_config: RollupConfig = rpc_client
                .request_noparams("optimism_rollupConfig")
                .await
                .context("Failed to fetch rollup config")?;
            println!("{}", hash_rollup_config(&rollup_config));
        }
    }

    Ok(())
}
