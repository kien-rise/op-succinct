use alloy_primitives::B256;
use clap::Parser;
use op_succinct_client_utils::boot::hash_rollup_config;
use op_succinct_elfs::AGGREGATION_ELF;
use op_succinct_host_utils::fetcher::OPSuccinctDataFetcher;
use op_succinct_proof_utils::get_range_elf_embedded;
use sp1_sdk::{utils, HashableKey, Prover, ProverClient};

use std::path::PathBuf;

#[derive(Debug, Clone, Parser)]
pub struct ConfigArgs {
    /// The environment file to use.
    #[arg(long)]
    pub env_file: Option<PathBuf>,

    /// Optional path to write output JSON results.
    #[arg(long)]
    pub output_json: Option<PathBuf>,
}

// Get the verification keys for the ELFs and check them against the contract.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = ConfigArgs::parse();
    if let Some(path) = args.env_file {
        dotenv::from_path(path)?;
    }
    utils::setup_logger();

    let prover = ProverClient::builder().cpu().build();
    let (_, range_vk) = prover.setup(get_range_elf_embedded());
    let range_vkey_commitment = B256::from(range_vk.hash_bytes());
    println!("Range Verification Key Hash: {}", &range_vkey_commitment);
    let (_, agg_vk) = prover.setup(AGGREGATION_ELF);
    let aggregation_vkey = agg_vk.bytes32();
    println!("Aggregation Verification Key Hash: {}", aggregation_vkey);

    let data_fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
    let rollup_config = data_fetcher.rollup_config.as_ref().unwrap();
    let rollup_config_hash = hash_rollup_config(rollup_config);
    println!("Rollup Config Hash: {}", &rollup_config_hash);

    if let Some(path) = args.output_json {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "range_vkey_commitment": range_vkey_commitment,
                "aggregation_vkey": aggregation_vkey,
                "rollup_config_hash": rollup_config_hash,
            }))?,
        )?;
    }

    Ok(())
}
