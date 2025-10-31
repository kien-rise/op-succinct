use std::path::PathBuf;

use alloy_primitives::B256;
use anyhow::Result;
use clap::Parser;
use op_succinct_client_utils::{boot::hash_rollup_config, types::u32_to_u8};
use op_succinct_elfs::AGGREGATION_ELF;
use op_succinct_host_utils::fetcher::OPSuccinctDataFetcher;
use op_succinct_proof_utils::get_range_elf_embedded;
use serde::Serialize;
use sp1_sdk::{utils, HashableKey, Prover, ProverClient};

#[derive(Debug, Clone, Parser)]
pub struct ConfigArgs {
    /// The environment file to use.
    #[arg(long)]
    pub env_file: Option<PathBuf>,

    /// Optional path to write output JSON results.
    #[arg(long)]
    pub output_json: Option<PathBuf>,
}

#[derive(Serialize)]
struct OutputData {
    range_vkey_commitment: String,
    aggregation_vkey: String,
    rollup_config_hash: String,
}

// Get the verification keys for the ELFs and check them against the contract.
#[tokio::main]
async fn main() -> Result<()> {
    let args = ConfigArgs::parse();

    if let Some(env_file) = args.env_file {
        dotenv::from_path(env_file)?;
    }
    utils::setup_logger();

    let prover = ProverClient::builder().cpu().build();

    let (_, range_vk) = prover.setup(get_range_elf_embedded());

    // Get the 32 byte commitment to the vkey from vkey.vk.hash_u32()
    let range_vkey_commitment = B256::from(u32_to_u8(range_vk.vk.hash_u32()));
    println!("Range Verification Key Hash: {range_vkey_commitment}");

    let (_, agg_vk) = prover.setup(AGGREGATION_ELF);
    println!("Aggregation Verification Key Hash: {}", agg_vk.bytes32());

    let data_fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
    let rollup_config = data_fetcher.rollup_config.as_ref().unwrap();
    let rollup_hash = hash_rollup_config(rollup_config);
    println!("Rollup Config Hash: 0x{:x}", rollup_hash);

    // Write to JSON file if requested
    if let Some(path) = args.output_json {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&OutputData {
                range_vkey_commitment: range_vkey_commitment.to_string(),
                aggregation_vkey: agg_vk.bytes32().to_string(),
                rollup_config_hash: format!("0x{:x}", rollup_hash),
            })?,
        )?;
        println!("Results written to {}", path.display());
    }

    Ok(())
}
