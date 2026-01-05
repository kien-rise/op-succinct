use std::{env, sync::Arc};

use alloy_primitives::BlockNumber;
use alloy_provider::RootProvider;
use anyhow::Result;
use fault_proof::{
    eigenda_provider::OnlineEigenDAPreimageProvider, range_optimizer::get_target_l2_block_number,
};
use kona_providers_alloy::{
    AlloyChainProvider, AlloyL2ChainProvider, OnlineBeaconClient, OnlineBlobProvider,
};
use op_succinct_host_utils::fetcher::OPSuccinctDataFetcher;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Get environment variables
    let l2_block_number: BlockNumber = env::var("L2_BLOCK_NUMBER")
        .expect("L2_BLOCK_NUMBER must be set")
        .parse()
        .expect("L2_BLOCK_NUMBER must be a valid block number");

    let l1_beacon_rpc = env::var("L1_BEACON_RPC").expect("L1_BEACON_RPC must be set");
    let eigenda_proxy_address =
        env::var("EIGENDA_PROXY_ADDRESS").expect("EIGENDA_PROXY_ADDRESS must be set");

    // Initialize fetcher with rollup config
    let fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;

    let rollup_config =
        Arc::new(fetcher.rollup_config.clone().expect("Rollup config must be initialized"));

    // Get L1 config from fetcher
    let l1_config = Arc::new(fetcher.l1_config.clone().expect("L1 config must be initialized"));

    // Create providers from the fetcher's RPC config
    let l1_chain_provider = AlloyChainProvider::new_http(fetcher.rpc_config.l1_rpc.clone(), 100);

    let beacon_client = OnlineBeaconClient::new_http(l1_beacon_rpc);
    let blob_provider = OnlineBlobProvider::init(beacon_client).await;

    let l2_chain_provider = AlloyL2ChainProvider::new(
        RootProvider::new(fetcher.rpc_config.l2_rpc_client()),
        rollup_config.clone(),
        100,
    );

    // Create EigenDA preimage provider
    let eigenda_preimage_provider =
        OnlineEigenDAPreimageProvider::new_http(eigenda_proxy_address.parse()?);

    tracing::info!("Starting range optimization from L2 block {}", l2_block_number);

    // Run the range optimizer
    let target_block = get_target_l2_block_number(
        rollup_config.clone(),
        l1_config,
        eigenda_preimage_provider,
        blob_provider,
        l1_chain_provider,
        l2_chain_provider,
        l2_block_number,
    )
    .await?;

    tracing::info!("Target L2 block number: {}", target_block);
    println!("Starting block: {}", l2_block_number);
    println!("Target block: {}", target_block);
    println!("Range: {} blocks", target_block - l2_block_number);

    Ok(())
}
