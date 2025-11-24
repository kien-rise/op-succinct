use std::{env, sync::Arc};

use alloy_primitives::Address;
use anyhow::Result;
use clap::Parser;
use fault_proof::{
    config::ProposerConfig, contract::DisputeGameFactory, prometheus::ProposerGauge,
    proposer::OPSuccinctProposer,
};
use op_succinct_host_utils::{
    fetcher::OPSuccinctDataFetcher,
    metrics::{init_metrics, MetricsGauge},
    setup_logger,
};
use op_succinct_proof_utils::initialize_host;
use op_succinct_signer_utils::SignerLock;
use tikv_jemallocator::Jemalloc;

#[global_allocator]
static ALLOCATOR: Jemalloc = Jemalloc;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = ".env.proposer")]
    env_file: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    dotenv::from_filename(args.env_file).ok();

    setup_logger();

    let proposer_config = ProposerConfig::from_env()?;
    let proposer_signer = SignerLock::from_env().await?;

    let l1_requests_per_second: Option<u32> = std::env::var("L1_REQUESTS_PER_SECOND")
        .expect("L1_REQUESTS_PER_SECOND must be set")
        .parse()
        .ok();
    let l1_max_retries: Option<u32> =
        std::env::var("L1_MAX_RETRIES").expect("L1_MAX_RETRIES must be set").parse().ok();
    let l1_provider = alloy_provider::RootProvider::new(kona_host::eth::rpc_client(
        proposer_config.l1_rpc.clone(),
        l1_requests_per_second,
        l1_max_retries,
    )?);

    let factory = DisputeGameFactory::new(
        env::var("FACTORY_ADDRESS")
            .expect("FACTORY_ADDRESS must be set")
            .parse::<Address>()
            .unwrap(),
        l1_provider.clone(),
    );

    let fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
    let host = initialize_host(Arc::new(fetcher.clone()));

    let proposer = Arc::new(
        OPSuccinctProposer::new(proposer_config, proposer_signer, factory, Arc::new(fetcher), host)
            .await
            .unwrap(),
    );

    // Initialize proposer gauges.
    ProposerGauge::register_all();

    // Initialize metrics exporter.
    init_metrics(&proposer.config.metrics_port);

    // Initialize the metrics gauges.
    ProposerGauge::init_all();

    proposer.run().await.expect("Runs in an infinite loop");

    Ok(())
}
