use std::sync::Arc;

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{BlockHash, BlockNumber};
use alloy_provider::RootProvider;
use alloy_rpc_client::RpcClient;
use alloy_transport_http::reqwest::Url;
use anyhow::{bail, Result};
use canoe_sp1_cc_host::canoe_proof_stdin;
use clap::{Parser, Subcommand};
use hokulea_host_bin::{
    cfg::SingleChainProvidersWithEigenDA, eigenda_preimage::OnlineEigenDAPreimageProvider,
};
use hokulea_proof::hint::ExtendedHintType;
use kona_host::{
    single::SingleChainProviders, MemoryKeyValueStore, OnlineHostBackend, SplitKeyValueStore,
};
use kona_proof::HintType;
use kona_providers_alloy::{OnlineBeaconClient, OnlineBlobProvider};
use rise_fp::{
    common::{
        args::{ClRpcArgs, L1RpcArgs, L2RpcArgs},
        primitives::RpcArgs,
    },
    proof::host::{PartialEigenDAWitnessData, RiseCfg, RiseHintHandler},
    rpc::{
        cl::{self, SafeDBClient},
        el,
    },
};
use sp1_sdk::SP1Stdin;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Prove {
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
    },
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
        Commands::Prove {
            claim_block,
            start_block,
            l1_rpc_args,
            l2_rpc_args,
            cl_rpc_args,
            l1_beacon_address,
            eigenda_proxy_address,
        } => {
            tracing::info!(%claim_block, %start_block, "Prove");

            let l1_rpc: RpcClient = RpcArgs::from(l1_rpc_args).into();
            let l2_rpc: RpcClient = RpcArgs::from(l2_rpc_args).into();
            let cl_rpc: RpcClient = RpcArgs::from(cl_rpc_args).into();

            let l1_head: BlockHash = {
                let mut safedb_client = SafeDBClient::new(cl_rpc.clone(), 64);
                match safedb_client.l2_to_l1_safe(claim_block, BlockNumberOrTag::Finalized).await? {
                    Some(num_hash) => num_hash.hash,
                    None => bail!("cannot find l1_safe"),
                }
            };

            let (agreed_l2_output_root, agreed_l2_head_hash) = {
                let resp = cl::get_output_at_block(&cl_rpc, start_block).await?;
                (resp.output_root, resp.block_ref.block_info.hash)
            };

            let (claimed_l2_output_root, claimed_l2_block_number) = {
                let resp = cl::get_output_at_block(&cl_rpc, claim_block).await?;
                assert!(resp.block_ref.block_info.number == claim_block);
                (resp.output_root, resp.block_ref.block_info.number)
            };

            let l2_chain_id = el::get_chain_id(&l2_rpc).await?;

            let rollup_config = Arc::new(cl::get_rollup_config(&cl_rpc).await?);

            let l1_config = match kona_registry::L1_CONFIGS.get(&rollup_config.l1_chain_id) {
                Some(l1_config) => Arc::new(l1_config.clone()),
                None => bail!("cannot find l1_config"),
            };

            // Create backend
            let backend = {
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

                let kv = {
                    let local_kv_store = cfg.get_local_key_value_store();
                    let mem_kv_store = MemoryKeyValueStore::new();
                    let split_kv_store = SplitKeyValueStore::new(local_kv_store, mem_kv_store);
                    Arc::new(RwLock::new(split_kv_store))
                };

                let providers = {
                    SingleChainProvidersWithEigenDA {
                        kona_providers: SingleChainProviders {
                            l1: RootProvider::new(l1_rpc.clone()),
                            l2: RootProvider::new(l2_rpc.clone()),
                            blobs: OnlineBlobProvider::init(OnlineBeaconClient::new_http(
                                l1_beacon_address,
                            ))
                            .await,
                        },
                        eigenda_preimage_provider: OnlineEigenDAPreimageProvider::new_http(
                            eigenda_proxy_address,
                        ),
                    }
                };

                OnlineHostBackend::new(cfg, kv, providers, RiseHintHandler)
                    .with_proactive_hint(ExtendedHintType::Original(HintType::L2PayloadWitness))
            };

            let partial_witness = PartialEigenDAWitnessData::build(backend, l1_rpc.clone()).await?;

            let canoe_inputs = partial_witness.get_canoe_inputs().await?;

            let canoe_proof_stdin = if canoe_inputs.is_empty() {
                SP1Stdin::new()
            } else {
                canoe_proof_stdin(&canoe_inputs, l1_rpc.clone()).await?
            };

            tracing::info!("partial_witness built");
        }
    }
    Ok(())
}
