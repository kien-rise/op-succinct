use std::{fmt::Debug, sync::Arc};

use alloy_primitives::Address;
use alloy_rpc_client::ClientBuilder;
use alloy_transport::layers::ThrottleLayer;
use alloy_transport_http::reqwest::Url;
use anyhow::Result;
use clap::Parser;
use tokio::{signal, sync::RwLock};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing_subscriber::EnvFilter;

use rise_fp::{
    common::state::State,
    components::{
        game_creator::{GameCreator, GameCreatorConfig},
        game_fetcher::{GameFetcher, GameFetcherConfig, GameFetcherRequest},
    },
};

#[derive(Debug, clap::Parser)]
struct Args {
    #[arg(long)]
    l1_rpc: Url,
    #[arg(long)]
    l1_max_rps: Option<u32>,
    #[arg(long)]
    factory_address: Address,
    #[arg(long)]
    registry_address: Address,
    #[command(flatten)]
    game_fetcher_config: GameFetcherConfig,
    #[command(flatten)]
    game_creator_config: GameCreatorConfig,
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

    let l1_rpc = match args.l1_max_rps {
        Some(rps) => ClientBuilder::default().layer(ThrottleLayer::new(rps)).http(args.l1_rpc),
        None => ClientBuilder::default().http(args.l1_rpc),
    };

    let state = Arc::new(RwLock::new(State::default()));

    let ct = CancellationToken::new();
    let tracker = TaskTracker::new();

    let game_fetcher_poll_interval = args.game_fetcher_config.poll_interval.clone();
    let game_fetcher = GameFetcher::new(
        state.clone(),
        args.game_fetcher_config,
        l1_rpc.clone(),
        args.factory_address,
        args.registry_address,
    );

    let game_creator = GameCreator::new(
        state.clone(),
        args.game_creator_config,
        l1_rpc.clone(),
        args.factory_address,
    );

    let (game_fetcher_tx, game_fetcher_rx) = GameFetcherRequest::channel();
    tracker.spawn(game_fetcher.start_dispatcher(ct.clone(), game_fetcher_rx));
    tracker.spawn(GameFetcher::start_driver(
        ct.clone(),
        game_fetcher_tx,
        game_fetcher_poll_interval,
    ));
    tracker.spawn(game_creator.start(ct.clone()));
    tracker.close();

    signal::ctrl_c().await?;
    ct.cancel();
    tracker.wait().await;
    Ok(())
}
