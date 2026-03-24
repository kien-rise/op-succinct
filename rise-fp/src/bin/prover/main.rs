use std::sync::Arc;

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{BlockHash, BlockNumber};
use alloy_provider::RootProvider;
use alloy_rpc_client::RpcClient;
use alloy_transport_http::reqwest::Url;
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use hokulea_host_bin::{
    cfg::SingleChainProvidersWithEigenDA, eigenda_preimage::OnlineEigenDAPreimageProvider,
};
use hokulea_proof::hint::ExtendedHintType;
use kona_host::{
    single::SingleChainProviders, MemoryKeyValueStore, OnlineHostBackend, PreimageServer,
    SplitKeyValueStore,
};
use kona_preimage::{BidirectionalChannel, HintReader, OracleServer};
use kona_proof::HintType;
use kona_providers_alloy::{OnlineBeaconClient, OnlineBlobProvider};
use op_succinct_eigenda_host_utils::witness_generator::EigenDAWitnessGenerator;
use rise_fp::{
    common::primitives::{derive_from, RpcArgs},
    proof::host::{RiseCfg, RiseHintHandler},
    rpc::{
        cl::{self, SafeDBClient},
        el,
    },
};
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, clap::Args)]
struct L1RpcArgs {
    #[arg(long = "l1.rpc-url", id = "l1.rpc-url")]
    rpc_url: Url,
    #[arg(long = "l1.max-rps", id = "l1.max-rps")]
    max_rps: Option<u32>,
}
derive_from!(L1RpcArgs, RpcArgs, [rpc_url, max_rps]);

#[derive(Debug, clap::Args)]
struct L2RpcArgs {
    #[arg(long = "l2.rpc-url", id = "l2.rpc-url")]
    rpc_url: Url,
    #[arg(long = "l2.max-rps", id = "l2.max-rps")]
    max_rps: Option<u32>,
}
derive_from!(L2RpcArgs, RpcArgs, [rpc_url, max_rps]);

#[derive(Debug, clap::Args)]
struct ClRpcArgs {
    #[arg(long = "cl.rpc-url", id = "cl.rpc-url")]
    rpc_url: Url,
    #[arg(long = "cl.max-rps", id = "cl.max-rps")]
    max_rps: Option<u32>,
}
derive_from!(ClRpcArgs, RpcArgs, [rpc_url, max_rps]);

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
                let start = cl::get_l1_origin(&cl_rpc, start_block).await?.number;
                let end = el::get_block_header(&l1_rpc, BlockNumberOrTag::Finalized).await?.number;
                let l1_number = safedb_client.l2_to_l1_safe(claim_block, start..end).await?;
                let l1_header = el::get_block_header(&l1_rpc, l1_number.into()).await?;
                l1_header.hash_slow()
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

            // Create channels
            let preimage = BidirectionalChannel::new()?;
            let hint = BidirectionalChannel::new()?;

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

                let hint_handler = RiseHintHandler {};

                OnlineHostBackend::new(cfg, kv, providers, hint_handler)
                    .with_proactive_hint(ExtendedHintType::Original(HintType::L2PayloadWitness))
            };

            let preimage_server = PreimageServer::new(
                OracleServer::new(preimage.host),
                HintReader::new(hint.host),
                Arc::new(backend),
            );

            let server_task = tokio::task::spawn(preimage_server.start());

            use op_succinct_host_utils::witness_generation::traits::WitnessGenerator;
            let witness_generator = EigenDAWitnessGenerator::new(l1_rpc.clone(), None);
            let witness_task = tokio::task::spawn(async move {
                witness_generator.run(preimage.client, hint.client).await
            });

            let witness = witness_task.await??;

            tracing::debug!(?witness, "witness");

            server_task.abort();
        }
    }
    Ok(())
}
