use std::{fmt::Debug, sync::Arc};

use alloy_primitives::Address;
use alloy_rpc_client::ClientBuilder;
use alloy_transport::layers::ThrottleLayer;
use alloy_transport_http::reqwest::Url;
use anyhow::Result;
use clap::Parser;
use tokio::{
    signal,
    sync::{Notify, RwLock},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing_subscriber::EnvFilter;

use rise_fp::{
    common::state::State,
    components::{
        app_driver::{AppDriver, AppDriverConfig},
        game_creator::{GameCreator, GameCreatorConfig},
        game_fetcher::{GameFetcher, GameFetcherConfig, GameFetcherRequest},
        tx_manager::{TxManager, TxManagerConfig, TxManagerRequest},
    },
};

#[derive(Debug, clap::Parser)]
struct Args {
    #[arg(long)]
    l1_rpc: Url,
    #[arg(long)]
    l1_max_rps: Option<u32>,
    #[arg(long)]
    l2_rpc: Url,
    #[arg(long)]
    l2_max_rps: Option<u32>,
    #[arg(long)]
    cl_rpc: Url,
    #[arg(long)]
    cl_max_rps: Option<u32>,
    #[arg(long)]
    factory_address: Address,
    #[arg(long)]
    registry_address: Address,
    #[command(flatten)]
    app_driver_config: AppDriverConfig,
    #[command(flatten)]
    game_fetcher_config: GameFetcherConfig,
    #[command(flatten)]
    game_creator_config: GameCreatorConfig,
    #[command(flatten)]
    tx_manager_config: TxManagerConfig,
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

    // Create RPC clients
    let l1_rpc = match args.l1_max_rps {
        Some(rps) => ClientBuilder::default().layer(ThrottleLayer::new(rps)).http(args.l1_rpc),
        None => ClientBuilder::default().http(args.l1_rpc),
    };

    let l2_rpc = match args.l2_max_rps {
        Some(rps) => ClientBuilder::default().layer(ThrottleLayer::new(rps)).http(args.l2_rpc),
        None => ClientBuilder::default().http(args.l2_rpc),
    };

    let cl_rpc = match args.cl_max_rps {
        Some(rps) => ClientBuilder::default().layer(ThrottleLayer::new(rps)).http(args.cl_rpc),
        None => ClientBuilder::default().http(args.cl_rpc),
    };

    // Create states, channels, notifications
    let state = Arc::new(RwLock::new(State::default()));
    let (game_fetcher_tx, game_fetcher_rx) = GameFetcherRequest::channel();
    let (tx_manager_tx, tx_manager_rx) = TxManagerRequest::channel();
    let game_fetcher_notification = Arc::new(Notify::new());

    // Create components
    let app_driver = AppDriver::new(args.app_driver_config, game_fetcher_tx);

    let game_fetcher = GameFetcher::new(
        args.game_fetcher_config,
        state.clone(),
        l1_rpc.clone(),
        args.factory_address,
        args.registry_address,
        game_fetcher_notification.clone(),
    );

    let tx_manager = TxManager::new(args.tx_manager_config, l1_rpc.clone());

    let game_creator = GameCreator::new(
        state.clone(),
        args.game_creator_config,
        l1_rpc.clone(),
        l2_rpc.clone(),
        cl_rpc.clone(),
        args.factory_address,
        tx_manager_tx,
    );

    // Start all the components
    let ct = CancellationToken::new();
    let tracker = TaskTracker::new();
    tracker.spawn(app_driver.start(ct.clone()));
    tracker.spawn(game_fetcher.start(ct.clone(), game_fetcher_rx));
    tracker.spawn(tx_manager.start(ct.clone(), tx_manager_rx));
    tracker.spawn(game_creator.start(ct.clone(), game_fetcher_notification));
    tracker.close();

    // Shutdown all the components
    signal::ctrl_c().await?;
    ct.cancel();
    tracker.wait().await;

    Ok(())
}
